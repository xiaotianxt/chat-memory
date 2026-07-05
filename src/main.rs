use rusqlite::{params, Connection, OpenFlags, OptionalExtension};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::env;
use std::ffi::OsString;
use std::fs;
use std::io::{self, IsTerminal as _, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{self, Command, Stdio};
use std::time::{SystemTime, UNIX_EPOCH};

const DEFAULT_CACHE: &str = "~/.cache/chat-memory/index.sqlite3";
const PY_FALLBACK: &str = "/Users/yupeit/bin/chat-memory-py";
const BRO_MCP_URL: &str = "http://127.0.0.1:3500/mcp";
const BRO_SETTINGS_PATH: &str = "~/.bro/settings.json";
const CHATGPT_MAIN_SCRIPT_ID: &str = "chat-memory-chatgpt-main";
const CHATGPT_SENDER_SCRIPT_ID: &str = "chat-memory-chatgpt-sender";

#[derive(Debug, Clone)]
struct Config {
    agent: String,
    cache: PathBuf,
    codex_home: PathBuf,
    opencode_db: PathBuf,
    command: CommandKind,
}

#[derive(Debug, Clone)]
enum CommandKind {
    Browse {
        query: String,
        limit: usize,
        enter: EnterAction,
        dump: bool,
        refresh: bool,
    },
    FzfSource {
        query: String,
        limit: usize,
    },
    Preview {
        agent: String,
        session_id: String,
        max_chars: usize,
    },
    Search {
        query: String,
        limit: usize,
    },
    List {
        limit: usize,
    },
    Count,
    Copy {
        session_id: String,
    },
    Resume {
        agent: String,
        session_id: String,
    },
    ChatgptIngest {
        file: PathBuf,
        account_id: String,
        workspace_id: String,
        source: String,
    },
    ChatgptSearch {
        query: String,
        limit: usize,
        account_id: Option<String>,
        workspace_id: Option<String>,
    },
    ChatgptDoctor,
    ChatgptServe {
        addr: String,
        token_file: PathBuf,
    },
    ChatgptUserscript {
        action: UserscriptAction,
        server: String,
        token_file: PathBuf,
        embed_token: bool,
    },
    Delegate(Vec<OsString>),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum EnterAction {
    View,
    Resume,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum UserscriptAction {
    Print,
    Install,
}

#[derive(Debug, Clone)]
struct Hit {
    agent: String,
    session_id: String,
    updated: String,
    title: String,
    directory: String,
    path: String,
    search: String,
}

fn main() {
    if let Err(err) = run() {
        eprintln!("memory: {err}");
        process::exit(1);
    }
}

fn run() -> Result<(), String> {
    let original_args: Vec<OsString> = env::args_os().skip(1).collect();
    if original_args
        .iter()
        .any(|arg| arg == "-h" || arg == "--help" || arg == "help")
    {
        print_help();
        return Ok(());
    }
    let cfg = parse_args(&original_args)?;
    match &cfg.command {
        CommandKind::Browse {
            query,
            limit,
            enter,
            dump,
            refresh,
        } => browse(&cfg, query, *limit, *enter, *dump, *refresh),
        CommandKind::FzfSource { query, limit } => print_fzf_source(&cfg, query, *limit),
        CommandKind::Preview {
            agent,
            session_id,
            max_chars,
        } => preview(&cfg, agent, session_id, *max_chars),
        CommandKind::Search { query, limit } => print_hits(&cfg, query, *limit),
        CommandKind::List { limit } => print_hits(&cfg, "", *limit),
        CommandKind::Count => count(&cfg),
        CommandKind::Copy { session_id } => copy_id(session_id),
        CommandKind::Resume { agent, session_id } => resume_or_view(&cfg, agent, session_id),
        CommandKind::ChatgptIngest {
            file,
            account_id,
            workspace_id,
            source,
        } => chatgpt_ingest(&cfg, file, account_id, workspace_id, source),
        CommandKind::ChatgptSearch {
            query,
            limit,
            account_id,
            workspace_id,
        } => chatgpt_search(
            &cfg,
            query,
            *limit,
            account_id.as_deref(),
            workspace_id.as_deref(),
        ),
        CommandKind::ChatgptDoctor => chatgpt_doctor(&cfg),
        CommandKind::ChatgptServe { addr, token_file } => chatgpt_serve(&cfg, addr, token_file),
        CommandKind::ChatgptUserscript {
            action,
            server,
            token_file,
            embed_token,
        } => chatgpt_userscript(&cfg, *action, server, token_file, *embed_token),
        CommandKind::Delegate(args) => delegate_to_python(args),
    }
}

fn parse_args(original: &[OsString]) -> Result<Config, String> {
    let mut agent = "all".to_string();
    let mut cache = expand_home(DEFAULT_CACHE);
    let mut codex_home =
        expand_home(&env::var("CODEX_HOME").unwrap_or_else(|_| "~/.codex".to_string()));
    let mut opencode_db = expand_home("~/.local/share/opencode/opencode.db");
    let mut raw = false;
    let mut positional: Vec<String> = Vec::new();
    let mut i = 0;
    while i < original.len() {
        let arg = original[i].to_string_lossy().to_string();
        match arg.as_str() {
            "--agent" => {
                i += 1;
                agent = get_value(original, i, "--agent")?;
            }
            "--cache" => {
                i += 1;
                cache = expand_home(&get_value(original, i, "--cache")?);
            }
            "--codex-home" => {
                i += 1;
                codex_home = expand_home(&get_value(original, i, "--codex-home")?);
            }
            "--opencode-db" => {
                i += 1;
                opencode_db = expand_home(&get_value(original, i, "--opencode-db")?);
            }
            "--raw" => raw = true,
            "-h" | "--help" => unreachable!("help is handled before parse_args"),
            other if other.starts_with("--agent=") => agent = other["--agent=".len()..].to_string(),
            other if other.starts_with("--cache=") => {
                cache = expand_home(&other["--cache=".len()..])
            }
            other if other.starts_with("--codex-home=") => {
                codex_home = expand_home(&other["--codex-home=".len()..])
            }
            other if other.starts_with("--opencode-db=") => {
                opencode_db = expand_home(&other["--opencode-db=".len()..])
            }
            _ => positional.push(arg),
        }
        i += 1;
    }

    let command = parse_command(&positional, original)?;
    let _ = raw;
    Ok(base_config(agent, cache, codex_home, opencode_db, command))
}

fn print_help() {
    println!(
        r#"chat-memory {}

Local-first search over Codex/OpenCode history and captured ChatGPT conversations.

USAGE:
  chat-memory [global options] <command> [command options]

GLOBAL OPTIONS:
  --cache PATH             Search/cache database path [default: ~/.cache/chat-memory/index.sqlite3]
  --agent all|codex|opencode
  --codex-home PATH
  --opencode-db PATH
  --raw
  -h, --help

COMMANDS:
  search <query>           Search local Codex/OpenCode history
  list                     List recent local sessions
  count                    Count local sessions
  browse [query]           Open interactive session browser

  chatgpt-ingest --file PATH
                           Ingest one ChatGPT conversation JSON export/payload
  chatgpt-search <query>   Search captured ChatGPT conversations
  chatgpt-doctor           Print ChatGPT cache diagnostics
  chatgpt-serve            Run loopback ingest service for browser capture
  chatgpt-userscript       Print or install ChatGPT capture userscripts through bro

CHATGPT SERVICE EXAMPLE:
  chat-memory --cache ~/.cache/chat-memory/index.sqlite3 chatgpt-serve \
    --addr 127.0.0.1:37531 \
    --token-file ~/.cache/chat-memory/chatgpt-ingest-token

HOMEBREW SERVICE:
  brew services start xiaotianxt/tap/chat-memory
  brew services stop xiaotianxt/tap/chat-memory
"#,
        env!("CARGO_PKG_VERSION")
    );
}

fn base_config(
    agent: String,
    cache: PathBuf,
    codex_home: PathBuf,
    opencode_db: PathBuf,
    command: CommandKind,
) -> Config {
    Config {
        agent,
        cache,
        codex_home,
        opencode_db,
        command,
    }
}

fn get_value(args: &[OsString], idx: usize, flag: &str) -> Result<String, String> {
    args.get(idx)
        .map(|s| s.to_string_lossy().to_string())
        .ok_or_else(|| format!("{flag} requires a value"))
}

fn parse_command(positional: &[String], original: &[OsString]) -> Result<CommandKind, String> {
    if positional.is_empty() {
        return Ok(CommandKind::Browse {
            query: String::new(),
            limit: 2000,
            enter: EnterAction::View,
            dump: false,
            refresh: false,
        });
    }

    let command_names = [
        "browse",
        "b",
        "fzf",
        "fzf-source",
        "preview",
        "search",
        "s",
        "list",
        "ls",
        "count",
        "copy",
        "resume",
        "refresh",
        "show",
        "path",
        "open",
        "chatgpt-ingest",
        "chatgpt-search",
        "chatgpt-doctor",
        "chatgpt-serve",
        "chatgpt-userscript",
    ];
    let first = positional[0].as_str();
    if !command_names.contains(&first) && !first.starts_with('-') {
        return parse_browse(positional, 0);
    }

    match first {
        "browse" | "b" | "fzf" => parse_browse(&positional[1..], 0),
        "fzf-source" => parse_fzf_source(&positional[1..]),
        "preview" => parse_preview(&positional[1..]),
        "search" | "s" => parse_search(&positional[1..]),
        "list" | "ls" => parse_list(&positional[1..]),
        "count" => Ok(CommandKind::Count),
        "copy" => parse_agent_session(&positional[1..], true),
        "resume" => parse_agent_session(&positional[1..], false),
        "chatgpt-ingest" => parse_chatgpt_ingest(&positional[1..]),
        "chatgpt-search" => parse_chatgpt_search(&positional[1..]),
        "chatgpt-doctor" => Ok(CommandKind::ChatgptDoctor),
        "chatgpt-serve" => parse_chatgpt_serve(&positional[1..]),
        "chatgpt-userscript" => parse_chatgpt_userscript(&positional[1..]),
        "refresh" | "show" | "path" | "open" => Ok(CommandKind::Delegate(original.to_vec())),
        _ if first.starts_with('-') => parse_browse(positional, 0),
        _ => Ok(CommandKind::Delegate(original.to_vec())),
    }
}

fn parse_browse(args: &[String], start: usize) -> Result<CommandKind, String> {
    let mut query = String::new();
    let mut limit = 2000usize;
    let mut enter = EnterAction::View;
    let mut dump = false;
    let mut refresh = false;
    let mut i = start;
    while i < args.len() {
        match args[i].as_str() {
            "-n" | "--limit" => {
                i += 1;
                limit = parse_usize(args.get(i), "--limit")?;
            }
            "--enter" => {
                i += 1;
                let value = args
                    .get(i)
                    .ok_or_else(|| "--enter requires a value".to_string())?;
                enter = match value.as_str() {
                    "view" => EnterAction::View,
                    "resume" => EnterAction::Resume,
                    _ => return Err("--enter must be view or resume".to_string()),
                };
            }
            "--dump" => dump = true,
            "--refresh" => refresh = true,
            value if value.starts_with('-') => {
                return Err(format!("unknown browse option: {value}"))
            }
            value => query = value.to_string(),
        }
        i += 1;
    }
    Ok(CommandKind::Browse {
        query,
        limit,
        enter,
        dump,
        refresh,
    })
}

fn parse_fzf_source(args: &[String]) -> Result<CommandKind, String> {
    let mut query = String::new();
    let mut limit = 2000usize;
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--query" => {
                i += 1;
                query = args.get(i).cloned().unwrap_or_default();
            }
            "--limit" | "-n" => {
                i += 1;
                limit = parse_usize(args.get(i), "--limit")?;
            }
            other => return Err(format!("unknown fzf-source option: {other}")),
        }
        i += 1;
    }
    Ok(CommandKind::FzfSource { query, limit })
}

fn parse_preview(args: &[String]) -> Result<CommandKind, String> {
    if args.len() < 2 {
        return Err("preview requires agent and session id".to_string());
    }
    let mut max_chars = 1200usize;
    let mut i = 2;
    while i < args.len() {
        match args[i].as_str() {
            "--max-chars" => {
                i += 1;
                max_chars = parse_usize(args.get(i), "--max-chars")?;
            }
            other => return Err(format!("unknown preview option: {other}")),
        }
        i += 1;
    }
    Ok(CommandKind::Preview {
        agent: args[0].clone(),
        session_id: args[1].clone(),
        max_chars,
    })
}

fn parse_search(args: &[String]) -> Result<CommandKind, String> {
    if args.is_empty() {
        return Err("search requires a query".to_string());
    }
    let mut query = args[0].clone();
    let mut limit = 20usize;
    let mut i = 1;
    while i < args.len() {
        match args[i].as_str() {
            "-n" | "--limit" => {
                i += 1;
                limit = parse_usize(args.get(i), "--limit")?;
            }
            value if !value.starts_with('-') => query.push_str(&format!(" {value}")),
            other => return Err(format!("unknown search option: {other}")),
        }
        i += 1;
    }
    Ok(CommandKind::Search { query, limit })
}

fn parse_list(args: &[String]) -> Result<CommandKind, String> {
    let mut limit = 20usize;
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "-n" | "--limit" => {
                i += 1;
                limit = parse_usize(args.get(i), "--limit")?;
            }
            other => return Err(format!("unknown list option: {other}")),
        }
        i += 1;
    }
    Ok(CommandKind::List { limit })
}

fn parse_agent_session(args: &[String], copy: bool) -> Result<CommandKind, String> {
    if args.len() < 2 {
        return Err("command requires agent and session id".to_string());
    }
    if copy {
        Ok(CommandKind::Copy {
            session_id: args[1].clone(),
        })
    } else {
        Ok(CommandKind::Resume {
            agent: args[0].clone(),
            session_id: args[1].clone(),
        })
    }
}

fn parse_chatgpt_ingest(args: &[String]) -> Result<CommandKind, String> {
    let mut file: Option<PathBuf> = None;
    let mut account_id = "default".to_string();
    let mut workspace_id = "default".to_string();
    let mut source = "manual".to_string();
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--account" => {
                i += 1;
                account_id = args
                    .get(i)
                    .cloned()
                    .ok_or_else(|| "--account requires a value".to_string())?;
            }
            "--workspace" => {
                i += 1;
                workspace_id = args
                    .get(i)
                    .cloned()
                    .ok_or_else(|| "--workspace requires a value".to_string())?;
            }
            "--source" => {
                i += 1;
                source = args
                    .get(i)
                    .cloned()
                    .ok_or_else(|| "--source requires a value".to_string())?;
            }
            value if value.starts_with('-') => {
                return Err(format!("unknown chatgpt-ingest option: {value}"))
            }
            value => {
                if file.is_some() {
                    return Err("chatgpt-ingest accepts one JSON file".to_string());
                }
                file = Some(PathBuf::from(value));
            }
        }
        i += 1;
    }
    Ok(CommandKind::ChatgptIngest {
        file: file.ok_or_else(|| "chatgpt-ingest requires a JSON file".to_string())?,
        account_id,
        workspace_id,
        source,
    })
}

fn parse_chatgpt_search(args: &[String]) -> Result<CommandKind, String> {
    if args.is_empty() {
        return Err("chatgpt-search requires a query".to_string());
    }
    let mut query_parts: Vec<String> = Vec::new();
    let mut limit = 20usize;
    let mut account_id = None;
    let mut workspace_id = None;
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "-n" | "--limit" => {
                i += 1;
                limit = parse_usize(args.get(i), "--limit")?;
            }
            "--account" => {
                i += 1;
                account_id = Some(
                    args.get(i)
                        .cloned()
                        .ok_or_else(|| "--account requires a value".to_string())?,
                );
            }
            "--workspace" => {
                i += 1;
                workspace_id = Some(
                    args.get(i)
                        .cloned()
                        .ok_or_else(|| "--workspace requires a value".to_string())?,
                );
            }
            value if value.starts_with('-') => {
                return Err(format!("unknown chatgpt-search option: {value}"))
            }
            value => query_parts.push(value.to_string()),
        }
        i += 1;
    }
    if query_parts.is_empty() {
        return Err("chatgpt-search requires a query".to_string());
    }
    Ok(CommandKind::ChatgptSearch {
        query: query_parts.join(" "),
        limit,
        account_id,
        workspace_id,
    })
}

fn parse_chatgpt_serve(args: &[String]) -> Result<CommandKind, String> {
    let mut addr = "127.0.0.1:37531".to_string();
    let mut token_file: Option<PathBuf> = None;
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--addr" => {
                i += 1;
                addr = args
                    .get(i)
                    .cloned()
                    .ok_or_else(|| "--addr requires a value".to_string())?;
            }
            "--token-file" => {
                i += 1;
                let raw = args
                    .get(i)
                    .cloned()
                    .ok_or_else(|| "--token-file requires a value".to_string())?;
                token_file = Some(expand_home(&raw));
            }
            value if value.starts_with('-') => {
                return Err(format!("unknown chatgpt-serve option: {value}"))
            }
            value => {
                return Err(format!(
                    "chatgpt-serve does not take positional args: {value}"
                ));
            }
        }
        i += 1;
    }
    let token_file = token_file.unwrap_or_else(|| {
        let parent = env::var_os("HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("."));
        parent.join(".cache/chat-memory/chatgpt-ingest-token")
    });
    if !addr.starts_with("127.0.0.1") && !addr.starts_with("localhost") {
        return Err(format!(
            "chatgpt-serve --addr must bind to loopback (127.0.0.1 or localhost): {addr}"
        ));
    }
    let addr = addr.replacen("localhost", "127.0.0.1", 1);
    Ok(CommandKind::ChatgptServe { addr, token_file })
}

fn parse_chatgpt_userscript(args: &[String]) -> Result<CommandKind, String> {
    if args.is_empty() {
        return Err("chatgpt-userscript requires a subcommand (print|install)".to_string());
    }
    let action = match args[0].as_str() {
        "print" => UserscriptAction::Print,
        "install" => UserscriptAction::Install,
        other => return Err(format!("unknown chatgpt-userscript subcommand: {other}")),
    };
    let mut server = "http://127.0.0.1:37531".to_string();
    let mut token_file: Option<PathBuf> = None;
    let mut embed_token = false;
    let mut i = 1;
    while i < args.len() {
        match args[i].as_str() {
            "--server" => {
                i += 1;
                server = args
                    .get(i)
                    .cloned()
                    .ok_or_else(|| "--server requires a value".to_string())?;
            }
            "--token-file" => {
                i += 1;
                let raw = args
                    .get(i)
                    .cloned()
                    .ok_or_else(|| "--token-file requires a value".to_string())?;
                token_file = Some(expand_home(&raw));
            }
            "--embed-token" => embed_token = true,
            value if value.starts_with('-') => {
                return Err(format!("unknown chatgpt-userscript option: {value}"))
            }
            value => return Err(format!("unexpected positional argument: {value}")),
        }
        i += 1;
    }
    let token_file = token_file.unwrap_or_else(|| {
        let parent = env::var_os("HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("."));
        parent.join(".cache/chat-memory/chatgpt-ingest-token")
    });
    Ok(CommandKind::ChatgptUserscript {
        action,
        server,
        token_file,
        embed_token,
    })
}

fn parse_usize(value: Option<&String>, flag: &str) -> Result<usize, String> {
    value
        .ok_or_else(|| format!("{flag} requires a value"))?
        .parse::<usize>()
        .map_err(|_| format!("{flag} must be an integer"))
}

fn expand_home(input: &str) -> PathBuf {
    if let Some(rest) = input.strip_prefix("~/") {
        if let Some(home) = env::var_os("HOME") {
            return PathBuf::from(home).join(rest);
        }
    }
    PathBuf::from(input)
}

fn open_cache(path: &Path) -> Result<Connection, String> {
    Connection::open_with_flags(
        path,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_URI,
    )
    .map_err(|err| format!("cannot open cache {}: {err}", path.display()))
}

fn ensure_cache(cfg: &Config) -> Result<(), String> {
    if cfg.cache.exists() {
        return Ok(());
    }
    delegate_to_python(&[
        OsString::from("--agent"),
        OsString::from(&cfg.agent),
        OsString::from("--cache"),
        cfg.cache.as_os_str().to_os_string(),
        OsString::from("--codex-home"),
        cfg.codex_home.as_os_str().to_os_string(),
        OsString::from("--opencode-db"),
        cfg.opencode_db.as_os_str().to_os_string(),
        OsString::from("refresh"),
    ])
}

fn query_hits(cfg: &Config, query: &str, limit: usize) -> Result<Vec<Hit>, String> {
    ensure_cache(cfg)?;
    let conn = open_cache(&cfg.cache)?;
    conn.busy_timeout(std::time::Duration::from_secs(2))
        .map_err(|err| err.to_string())?;
    let mut sql = String::from(
        "SELECT agent, session_id, updated, title, directory, path, search FROM sessions",
    );
    let mut clauses: Vec<String> = Vec::new();
    let mut params_vec: Vec<String> = Vec::new();
    if cfg.agent != "all" {
        clauses.push("agent = ?".to_string());
        params_vec.push(cfg.agent.clone());
    }
    for term in query.split_whitespace() {
        clauses.push(
            "lower(session_id || ' ' || title || ' ' || directory || ' ' || search) LIKE ?"
                .to_string(),
        );
        params_vec.push(format!("%{}%", term.to_lowercase()));
    }
    if !clauses.is_empty() {
        sql.push_str(" WHERE ");
        sql.push_str(&clauses.join(" AND "));
    }
    sql.push_str(" ORDER BY updated_ts DESC LIMIT ?");
    params_vec.push(limit.to_string());

    let mut stmt = conn.prepare(&sql).map_err(|err| err.to_string())?;
    let rows = stmt
        .query_map(rusqlite::params_from_iter(params_vec.iter()), |row| {
            Ok(Hit {
                agent: row.get(0)?,
                session_id: row.get(1)?,
                updated: row.get(2)?,
                title: row.get(3)?,
                directory: row.get(4)?,
                path: row.get(5)?,
                search: row.get(6)?,
            })
        })
        .map_err(|err| err.to_string())?;
    let mut hits = Vec::new();
    for row in rows {
        hits.push(row.map_err(|err| err.to_string())?);
    }
    Ok(hits)
}

fn get_hit(cfg: &Config, agent: &str, session_id: &str) -> Result<Option<Hit>, String> {
    ensure_cache(cfg)?;
    let conn = open_cache(&cfg.cache)?;
    let mut stmt = conn
        .prepare(
            "SELECT agent, session_id, updated, title, directory, path, search
             FROM sessions WHERE agent = ? AND session_id = ?",
        )
        .map_err(|err| err.to_string())?;
    let mut rows = stmt
        .query(params![agent, session_id])
        .map_err(|err| err.to_string())?;
    if let Some(row) = rows.next().map_err(|err| err.to_string())? {
        return Ok(Some(Hit {
            agent: row.get(0).map_err(|err| err.to_string())?,
            session_id: row.get(1).map_err(|err| err.to_string())?,
            updated: row.get(2).map_err(|err| err.to_string())?,
            title: row.get(3).map_err(|err| err.to_string())?,
            directory: row.get(4).map_err(|err| err.to_string())?,
            path: row.get(5).map_err(|err| err.to_string())?,
            search: row.get(6).map_err(|err| err.to_string())?,
        }));
    }
    Ok(None)
}

fn browse(
    cfg: &Config,
    query: &str,
    limit: usize,
    enter: EnterAction,
    dump: bool,
    refresh: bool,
) -> Result<(), String> {
    if refresh {
        delegate_to_python(&python_refresh_args(cfg))?;
    }
    let hits = query_hits(cfg, query, limit)?;
    if dump {
        for hit in &hits {
            print_hit_block(hit);
        }
        return Ok(());
    }
    let fzf = which("fzf").ok_or_else(|| "fzf not found".to_string())?;
    let exe = env::current_exe().map_err(|err| err.to_string())?;
    let source = shell_words(&[
        exe.to_string_lossy().as_ref(),
        "--agent",
        &cfg.agent,
        "--cache",
        &cfg.cache.to_string_lossy(),
        "fzf-source",
        "--limit",
        &limit.to_string(),
        "--query",
        "{q}",
    ]);
    let preview = shell_words(&[
        exe.to_string_lossy().as_ref(),
        "--agent",
        &cfg.agent,
        "--cache",
        &cfg.cache.to_string_lossy(),
        "preview",
        "{1}",
        "{2}",
    ]);
    let input = hits.iter().map(fzf_row).collect::<Vec<_>>().join("\n");
    let mut child = Command::new(fzf)
        .arg("--ansi")
        .arg("--delimiter=\t")
        .arg("--with-nth=3")
        .arg("--disabled")
        .arg("--no-sort")
        .arg("--prompt=memory> ")
        .arg("--height=90%")
        .arg("--layout=reverse")
        .arg("--border")
        .arg("--preview")
        .arg(preview)
        .arg("--preview-window=right:65%:wrap:hidden")
        .arg("--bind")
        .arg("ctrl-p:toggle-preview")
        .arg("--bind")
        .arg(format!("change:reload:{source}"))
        .arg("--expect=enter,ctrl-r,ctrl-v,ctrl-y")
        .arg("--header=Type to search cached text | Enter: view | Ctrl-P: preview | Ctrl-R: codex resume | Ctrl-Y: copy id")
        .args(if query.is_empty() { Vec::new() } else { vec!["--query".to_string(), query.to_string()] })
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .map_err(|err| format!("failed to start fzf: {err}"))?;
    {
        let stdin = child
            .stdin
            .as_mut()
            .ok_or_else(|| "failed to open fzf stdin".to_string())?;
        stdin
            .write_all(input.as_bytes())
            .map_err(|err| err.to_string())?;
    }
    let output = child.wait_with_output().map_err(|err| err.to_string())?;
    if !output.status.success() {
        return Ok(());
    }
    let stdout = String::from_utf8_lossy(&output.stdout);
    let mut lines = stdout.lines();
    let first = lines.next().unwrap_or("");
    let (key, selected) = if matches!(first, "" | "enter" | "ctrl-r" | "ctrl-v" | "ctrl-y") {
        (
            if first.is_empty() { "enter" } else { first },
            lines.next().unwrap_or(""),
        )
    } else {
        ("enter", first)
    };
    if selected.is_empty() {
        return Ok(());
    }
    let fields: Vec<&str> = selected.split('\t').collect();
    if fields.len() < 2 {
        return Err("invalid fzf selection".to_string());
    }
    match key {
        "ctrl-y" => copy_id(fields[1]),
        "ctrl-r" => resume_or_view(cfg, fields[0], fields[1]),
        _ if enter == EnterAction::Resume => resume_or_view(cfg, fields[0], fields[1]),
        _ => view_session(cfg, fields[0], fields[1]),
    }
}

fn print_fzf_source(cfg: &Config, query: &str, limit: usize) -> Result<(), String> {
    for hit in query_hits(cfg, query, limit)? {
        println!("{}", fzf_row(&hit));
    }
    Ok(())
}

fn preview(cfg: &Config, agent: &str, session_id: &str, max_chars: usize) -> Result<(), String> {
    if let Some(hit) = get_hit(cfg, agent, session_id)? {
        println!("{}", display_line(&hit));
        println!("id: {}", hit.session_id);
        if !hit.directory.is_empty() {
            println!("cwd: {}", hit.directory);
        }
        if !hit.path.is_empty() {
            println!("path: {}", hit.path);
        }
        println!();
        println!("{}", clip(&hit.search, max_chars));
    }
    Ok(())
}

fn print_hits(cfg: &Config, query: &str, limit: usize) -> Result<(), String> {
    for hit in query_hits(cfg, query, limit)? {
        print_hit_block(&hit);
    }
    Ok(())
}

fn count(cfg: &Config) -> Result<(), String> {
    ensure_cache(cfg)?;
    let conn = open_cache(&cfg.cache)?;
    let mut stmt = conn
        .prepare("SELECT agent, COUNT(*) FROM sessions GROUP BY agent ORDER BY agent")
        .map_err(|err| err.to_string())?;
    let rows = stmt
        .query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?))
        })
        .map_err(|err| err.to_string())?;
    for row in rows {
        let (agent, count) = row.map_err(|err| err.to_string())?;
        println!("{agent} {count}");
    }
    Ok(())
}

fn print_hit_block(hit: &Hit) {
    println!("{}  {:<19}  {}", hit.agent, hit.updated, hit.session_id);
    if !hit.title.is_empty() {
        println!("  title: {}", clip(&hit.title, 180));
    }
    if !hit.directory.is_empty() {
        println!("  cwd:   {}", hit.directory);
    }
    println!();
}

fn fzf_row(hit: &Hit) -> String {
    format!("{}\t{}\t{}", hit.agent, hit.session_id, display_line(hit))
}

fn display_line(hit: &Hit) -> String {
    let updated = if hit.updated.len() > 16 {
        &hit.updated[..16]
    } else {
        &hit.updated
    };
    format!(
        "{:<8} {:<16} {:<94} {}",
        hit.agent,
        updated,
        clip(&hit.title, 92),
        short_path(&hit.directory)
    )
}

fn short_path(path: &str) -> String {
    if path.is_empty() {
        return String::new();
    }
    let home = env::var("HOME").unwrap_or_default();
    let mut value = if !home.is_empty() && path.starts_with(&home) {
        format!("~{}", &path[home.len()..])
    } else {
        path.to_string()
    };
    let prefix = if value.starts_with("~/") { "~/" } else { "" };
    if value.starts_with("~/") {
        value = value[2..].to_string();
    }
    let parts: Vec<&str> = value.split('/').filter(|s| !s.is_empty()).collect();
    if parts.len() <= 3 {
        return format!("{prefix}{}", parts.join("/"));
    }
    format!("{prefix}.../{}", parts[parts.len() - 3..].join("/"))
}

fn clip(text: &str, limit: usize) -> String {
    let one_line = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if one_line.chars().count() <= limit {
        return one_line;
    }
    let mut out = one_line
        .chars()
        .take(limit.saturating_sub(3))
        .collect::<String>();
    out.push_str("...");
    out
}

fn copy_id(session_id: &str) -> Result<(), String> {
    if let Some(pbcopy) = which("pbcopy") {
        let mut child = Command::new(pbcopy)
            .stdin(Stdio::piped())
            .spawn()
            .map_err(|err| err.to_string())?;
        if let Some(stdin) = child.stdin.as_mut() {
            stdin
                .write_all(session_id.as_bytes())
                .map_err(|err| err.to_string())?;
        }
        let _ = child.wait();
    }
    println!("{session_id}");
    Ok(())
}

fn resume_or_view(cfg: &Config, agent: &str, session_id: &str) -> Result<(), String> {
    if agent == "codex" {
        if let Some(codex) = which("codex") {
            let err = Command::new(codex)
                .arg("resume")
                .arg(session_id)
                .status()
                .map_err(|err| err.to_string())?;
            if err.success() {
                return Ok(());
            }
        }
    }
    view_session(cfg, agent, session_id)
}

fn view_session(cfg: &Config, agent: &str, session_id: &str) -> Result<(), String> {
    let output = Command::new(python_path())
        .arg("--agent")
        .arg(agent)
        .arg("--cache")
        .arg(&cfg.cache)
        .arg("--codex-home")
        .arg(&cfg.codex_home)
        .arg("--opencode-db")
        .arg(&cfg.opencode_db)
        .arg("show")
        .arg(session_id)
        .arg("--max-chars")
        .arg("5000")
        .output()
        .map_err(|err| err.to_string())?;
    if !output.status.success() {
        io::stderr()
            .write_all(&output.stderr)
            .map_err(|err| err.to_string())?;
        return Err("show failed".to_string());
    }
    let text = String::from_utf8_lossy(&output.stdout);
    if io::stdout().is_terminal() {
        page(&text)
    } else {
        print!("{text}");
        Ok(())
    }
}

fn page(text: &str) -> Result<(), String> {
    let pager = env::var("PAGER").unwrap_or_else(|_| "less -R".to_string());
    let mut parts = pager.split_whitespace();
    let Some(cmd) = parts.next() else {
        print!("{text}");
        return Ok(());
    };
    let mut child = Command::new(cmd)
        .args(parts)
        .stdin(Stdio::piped())
        .spawn()
        .map_err(|_| {
            print!("{text}");
            "pager unavailable".to_string()
        })?;
    if let Some(stdin) = child.stdin.as_mut() {
        stdin
            .write_all(text.as_bytes())
            .map_err(|err| err.to_string())?;
    }
    let _ = child.wait();
    Ok(())
}

fn delegate_to_python(args: &[OsString]) -> Result<(), String> {
    let status = Command::new(python_path())
        .args(args)
        .status()
        .map_err(|err| format!("failed to run Python fallback: {err}"))?;
    if status.success() {
        Ok(())
    } else {
        Err(format!("Python fallback exited with {status}"))
    }
}

fn python_refresh_args(cfg: &Config) -> Vec<OsString> {
    vec![
        OsString::from("--agent"),
        OsString::from(&cfg.agent),
        OsString::from("--cache"),
        cfg.cache.as_os_str().to_os_string(),
        OsString::from("--codex-home"),
        cfg.codex_home.as_os_str().to_os_string(),
        OsString::from("--opencode-db"),
        cfg.opencode_db.as_os_str().to_os_string(),
        OsString::from("refresh"),
    ]
}

fn python_path() -> PathBuf {
    env::var_os("CHAT_MEMORY_PY")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(PY_FALLBACK))
}

fn shell_words(parts: &[&str]) -> String {
    parts
        .iter()
        .map(|part| shell_quote(part))
        .collect::<Vec<_>>()
        .join(" ")
}

fn shell_quote(value: &str) -> String {
    if value.chars().all(|c| {
        c.is_ascii_alphanumeric() || matches!(c, '/' | '-' | '_' | '.' | ':' | '=' | '{' | '}')
    }) {
        return value.to_string();
    }
    format!("'{}'", value.replace('\'', "'\\''"))
}

fn which(name: &str) -> Option<PathBuf> {
    let path = env::var_os("PATH")?;
    env::split_paths(&path)
        .map(|dir| dir.join(name))
        .find(|candidate| candidate.is_file())
}

// ===== ChatGPT local search slice =====

fn chatgpt_db_path(cfg: &Config) -> PathBuf {
    if let Some(custom) = env::var_os("CHATGPT_SEARCH_DB") {
        return expand_home(&custom.to_string_lossy());
    }
    cfg.cache
        .parent()
        .unwrap_or_else(|| Path::new("."))
        .join("chatgpt.sqlite3")
}

fn open_chatgpt_db(path: &Path) -> Result<Connection, String> {
    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            fs::create_dir_all(parent).map_err(|err| {
                format!("cannot create chatgpt db dir {}: {err}", parent.display())
            })?;
        }
    }
    let conn = Connection::open(path)
        .map_err(|err| format!("cannot open chatgpt db {}: {err}", path.display()))?;
    conn.execute_batch("PRAGMA journal_mode = WAL; PRAGMA foreign_keys = ON;")
        .map_err(|err| err.to_string())?;
    ensure_chatgpt_schema(&conn)?;
    Ok(conn)
}

fn ensure_chatgpt_schema(conn: &Connection) -> Result<(), String> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS conversations (
            conversation_pk INTEGER PRIMARY KEY AUTOINCREMENT,
            account_id TEXT NOT NULL,
            workspace_id TEXT NOT NULL,
            remote_conversation_id TEXT NOT NULL,
            title TEXT NOT NULL DEFAULT '',
            created_at_remote REAL,
            updated_at_remote REAL,
            last_message_at_remote REAL,
            last_seen_in_list_at REAL,
            last_fetched_at REAL,
            last_indexed_at REAL,
            current_snapshot_hash TEXT,
            freshness_state TEXT NOT NULL DEFAULT 'unknown',
            priority_bucket TEXT NOT NULL DEFAULT 'warm',
            etag TEXT,
            last_modified TEXT,
            remote_version TEXT,
            last_error TEXT,
            retry_after_at REAL,
            consecutive_failures INTEGER NOT NULL DEFAULT 0,
            visibility_state TEXT NOT NULL DEFAULT 'unknown',
            UNIQUE(account_id, workspace_id, remote_conversation_id)
        );
        CREATE TABLE IF NOT EXISTS conversation_snapshots (
            snapshot_hash TEXT NOT NULL,
            conversation_pk INTEGER NOT NULL,
            fetched_at REAL NOT NULL,
            schema_version INTEGER NOT NULL,
            source TEXT NOT NULL,
            json_blob TEXT NOT NULL,
            json_size_bytes INTEGER NOT NULL,
            message_count INTEGER NOT NULL,
            max_message_time REAL,
            PRIMARY KEY(conversation_pk, snapshot_hash),
            FOREIGN KEY (conversation_pk) REFERENCES conversations(conversation_pk) ON DELETE CASCADE
        );
        CREATE TABLE IF NOT EXISTS messages (
            conversation_pk INTEGER NOT NULL,
            message_id TEXT NOT NULL,
            parent_message_id TEXT,
            role TEXT NOT NULL,
            content_type TEXT,
            created_at_remote REAL,
            updated_at_remote REAL,
            text TEXT NOT NULL DEFAULT '',
            text_hash TEXT,
            snapshot_hash TEXT,
            is_current INTEGER NOT NULL DEFAULT 1,
            PRIMARY KEY(conversation_pk, message_id),
            FOREIGN KEY (conversation_pk) REFERENCES conversations(conversation_pk) ON DELETE CASCADE
        );
        CREATE TABLE IF NOT EXISTS search_documents (
            doc_id INTEGER PRIMARY KEY AUTOINCREMENT,
            conversation_pk INTEGER NOT NULL,
            message_id TEXT,
            chunk_ord INTEGER NOT NULL DEFAULT 0,
            title TEXT NOT NULL DEFAULT '',
            text TEXT NOT NULL DEFAULT '',
            text_ngram TEXT NOT NULL DEFAULT '',
            indexed_at REAL NOT NULL,
            snapshot_hash TEXT,
            FOREIGN KEY (conversation_pk) REFERENCES conversations(conversation_pk) ON DELETE CASCADE
        );
        CREATE INDEX IF NOT EXISTS idx_search_documents_conv ON search_documents(conversation_pk);
        CREATE INDEX IF NOT EXISTS idx_search_documents_text ON search_documents(text);
        CREATE TABLE IF NOT EXISTS refresh_queue (
            conversation_pk INTEGER NOT NULL,
            reason TEXT NOT NULL,
            priority INTEGER NOT NULL DEFAULT 0,
            not_before REAL,
            attempt_count INTEGER NOT NULL DEFAULT 0,
            PRIMARY KEY(conversation_pk, reason),
            FOREIGN KEY (conversation_pk) REFERENCES conversations(conversation_pk) ON DELETE CASCADE
        );
        CREATE TABLE IF NOT EXISTS tombstones (
            conversation_pk INTEGER PRIMARY KEY,
            account_id TEXT NOT NULL,
            workspace_id TEXT NOT NULL,
            remote_conversation_id TEXT NOT NULL,
            last_known_title TEXT,
            deleted_or_inaccessible_at REAL NOT NULL,
            reason TEXT NOT NULL
        );
        CREATE TABLE IF NOT EXISTS service_state (
            key TEXT PRIMARY KEY,
            value TEXT NOT NULL
        );
        CREATE TABLE IF NOT EXISTS refresh_leases (
            lease_id TEXT PRIMARY KEY,
            conversation_pk INTEGER,
            lease_type TEXT NOT NULL,
            url TEXT NOT NULL,
            granted_at REAL NOT NULL,
            deadline_at REAL NOT NULL,
            completed_at REAL,
            status TEXT NOT NULL DEFAULT 'active',
            last_error TEXT,
            FOREIGN KEY (conversation_pk) REFERENCES conversations(conversation_pk) ON DELETE CASCADE
        );
        CREATE INDEX IF NOT EXISTS idx_refresh_leases_status ON refresh_leases(status);
        ",
    )
    .map_err(|err| format!("chatgpt schema init failed: {err}"))?;
    // Additive migration for pre-v2 databases that already have a conversations
    // table with fewer columns. Inspect PRAGMA table_info and ALTER TABLE ADD
    // COLUMN for any missing nullable/defaulted columns. Never drop/rebuild.
    migrate_conversations_columns(conn)?;
    migrate_refresh_queue_columns(conn)?;
    conn.execute_batch(
        "CREATE INDEX IF NOT EXISTS idx_refresh_queue_due
         ON refresh_queue(priority DESC, not_before ASC);",
    )
    .map_err(|err| format!("chatgpt schema index init failed: {err}"))?;
    Ok(())
}

/// Return true if `table` has a column named `column` (case-insensitive).
fn column_exists(conn: &Connection, table: &str, column: &str) -> Result<bool, String> {
    let cols: Vec<String> = conn
        .prepare(&format!("PRAGMA table_info({table})"))
        .map_err(|e| e.to_string())?
        .query_map([], |r| r.get::<_, String>(1))
        .map_err(|e| e.to_string())?
        .filter_map(Result::ok)
        .collect();
    Ok(cols.iter().any(|c| c.eq_ignore_ascii_case(column)))
}

/// Add a missing column to an existing table via ALTER TABLE ADD COLUMN.
fn ensure_column(conn: &Connection, table: &str, column: &str, decl: &str) -> Result<(), String> {
    if !column_exists(conn, table, column)? {
        conn.execute(
            &format!("ALTER TABLE {table} ADD COLUMN {column} {decl}"),
            [],
        )
        .map_err(|e| format!("migrate {table}.{column}: {e}"))?;
    }
    Ok(())
}

fn table_exists(conn: &Connection, table: &str) -> Result<bool, String> {
    let n: i64 = conn
        .query_row(
            &format!("SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name='{table}'"),
            [],
            |r| r.get(0),
        )
        .map_err(|e| e.to_string())?;
    Ok(n > 0)
}

/// Additively migrate an older `conversations` table to the current column set.
fn migrate_conversations_columns(conn: &Connection) -> Result<(), String> {
    if !table_exists(conn, "conversations")? {
        return Ok(());
    }
    ensure_column(conn, "conversations", "title", "TEXT NOT NULL DEFAULT ''")?;
    ensure_column(conn, "conversations", "created_at_remote", "REAL")?;
    ensure_column(conn, "conversations", "updated_at_remote", "REAL")?;
    ensure_column(conn, "conversations", "last_message_at_remote", "REAL")?;
    ensure_column(conn, "conversations", "last_seen_in_list_at", "REAL")?;
    ensure_column(conn, "conversations", "last_fetched_at", "REAL")?;
    ensure_column(conn, "conversations", "last_indexed_at", "REAL")?;
    ensure_column(conn, "conversations", "current_snapshot_hash", "TEXT")?;
    ensure_column(
        conn,
        "conversations",
        "freshness_state",
        "TEXT NOT NULL DEFAULT 'unknown'",
    )?;
    ensure_column(
        conn,
        "conversations",
        "priority_bucket",
        "TEXT NOT NULL DEFAULT 'warm'",
    )?;
    ensure_column(conn, "conversations", "etag", "TEXT")?;
    ensure_column(conn, "conversations", "last_modified", "TEXT")?;
    ensure_column(conn, "conversations", "remote_version", "TEXT")?;
    ensure_column(conn, "conversations", "last_error", "TEXT")?;
    ensure_column(conn, "conversations", "retry_after_at", "REAL")?;
    ensure_column(
        conn,
        "conversations",
        "consecutive_failures",
        "INTEGER NOT NULL DEFAULT 0",
    )?;
    ensure_column(
        conn,
        "conversations",
        "visibility_state",
        "TEXT NOT NULL DEFAULT 'unknown'",
    )?;
    Ok(())
}

/// Additively migrate an older `refresh_queue` table to the current column set.
fn migrate_refresh_queue_columns(conn: &Connection) -> Result<(), String> {
    if !table_exists(conn, "refresh_queue")? {
        return Ok(());
    }
    ensure_column(
        conn,
        "refresh_queue",
        "priority",
        "INTEGER NOT NULL DEFAULT 0",
    )?;
    ensure_column(conn, "refresh_queue", "not_before", "REAL")?;
    ensure_column(
        conn,
        "refresh_queue",
        "attempt_count",
        "INTEGER NOT NULL DEFAULT 0",
    )?;
    Ok(())
}
fn now_secs() -> f64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs_f64())
        .unwrap_or(0.0)
}

fn sha256_hex(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    hasher
        .finalize()
        .iter()
        .map(|b| format!("{:02x}", b))
        .collect::<String>()
}

struct NormalizedMessage {
    message_id: String,
    parent_message_id: Option<String>,
    role: String,
    content_type: String,
    created_at: Option<f64>,
    updated_at: Option<f64>,
    text: String,
}

struct NormalizedConversation {
    remote_conversation_id: String,
    title: String,
    created_at: Option<f64>,
    updated_at: Option<f64>,
    messages: Vec<NormalizedMessage>,
    max_message_time: Option<f64>,
}

fn as_f64(v: &Value) -> Option<f64> {
    v.as_f64()
        .or_else(|| v.as_str().and_then(|s| s.parse::<f64>().ok()))
}

fn parts_to_text(parts: &Value) -> String {
    let mut out = Vec::new();
    if let Some(arr) = parts.as_array() {
        for p in arr {
            if let Some(s) = p.as_str() {
                if !s.is_empty() {
                    out.push(s.to_string());
                }
            } else if p.is_string() {
                // handled above; ignore non-string parts (e.g. image refs)
            }
        }
    } else if let Some(s) = parts.as_str() {
        if !s.is_empty() {
            out.push(s.to_string());
        }
    }
    out.join("\n")
}

fn normalize_chatgpt_json(raw: &Value) -> Result<NormalizedConversation, String> {
    let remote_conversation_id = raw
        .get("conversation_id")
        .and_then(|v| v.as_str())
        .or_else(|| raw.get("id").and_then(|v| v.as_str()))
        .map(|s| s.to_string())
        .ok_or_else(|| "conversation JSON missing conversation_id/id".to_string())?;

    let title = raw
        .get("title")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let created_at = raw.get("create_time").and_then(as_f64);
    let updated_at = raw.get("update_time").and_then(as_f64);

    let mapping = raw
        .get("mapping")
        .and_then(|v| v.as_object())
        .ok_or_else(|| "conversation JSON missing mapping object".to_string())?;

    let mut messages = Vec::new();
    let mut max_message_time: Option<f64> = None;
    for (_, node) in mapping.iter() {
        let node_obj = match node.as_object() {
            Some(o) => o,
            None => continue,
        };
        let parent = node_obj
            .get("parent")
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
            .map(|s| s.to_string());

        let message = match node_obj.get("message").and_then(|v| v.as_object()) {
            Some(m) => m,
            None => continue,
        };

        let message_id = message
            .get("id")
            .and_then(|v| v.as_str())
            .or_else(|| node_obj.get("id").and_then(|v| v.as_str()))
            .map(|s| s.to_string())
            .unwrap_or_else(|| {
                // fall back to node id if present, else synthetic
                node_obj
                    .get("id")
                    .and_then(|v| v.as_str())
                    .map(|s| s.to_string())
                    .unwrap_or_else(|| format!("node-{}", messages.len()))
            });

        let role = message
            .get("author")
            .and_then(|a| a.get("role"))
            .and_then(|v| v.as_str())
            .unwrap_or("unknown")
            .to_string();

        let content = message.get("content").and_then(|v| v.as_object());
        let content_type = content
            .and_then(|c| c.get("content_type"))
            .and_then(|v| v.as_str())
            .unwrap_or("text")
            .to_string();
        let parts = content.and_then(|c| c.get("parts"));
        let text = match parts {
            Some(p) => parts_to_text(p),
            None => content
                .and_then(|c| c.get("text"))
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string(),
        };

        let created_at = message.get("create_time").and_then(as_f64);
        let updated_at = message.get("update_time").and_then(as_f64);

        if let Some(t) = created_at {
            max_message_time = Some(max_message_time.map_or(t, |m| m.max(t)));
        }

        messages.push(NormalizedMessage {
            message_id,
            parent_message_id: parent,
            role,
            content_type,
            created_at,
            updated_at,
            text,
        });
    }

    // stable ordering by create time then message id
    messages.sort_by(|a, b| {
        a.created_at
            .unwrap_or(0.0)
            .partial_cmp(&b.created_at.unwrap_or(0.0))
            .unwrap_or(std::cmp::Ordering::Equal)
            .then(a.message_id.cmp(&b.message_id))
    });

    Ok(NormalizedConversation {
        remote_conversation_id,
        title,
        created_at,
        updated_at,
        messages,
        max_message_time,
    })
}

fn chunk_text(text: &str) -> Vec<String> {
    const MAX: usize = 2000;
    if text.chars().count() <= MAX {
        return vec![text.to_string()];
    }
    let mut chunks: Vec<String> = Vec::new();
    let mut buf = String::new();
    for para in text.split("\n\n") {
        let para = para.trim();
        if para.is_empty() {
            continue;
        }
        if para.chars().count() > MAX {
            if !buf.is_empty() {
                chunks.push(std::mem::take(&mut buf));
            }
            let mut line_buf = String::new();
            for line in para.split('\n') {
                if line.chars().count() > MAX {
                    if !line_buf.is_empty() {
                        chunks.push(std::mem::take(&mut line_buf));
                    }
                    let mut acc = String::new();
                    for ch in line.chars() {
                        acc.push(ch);
                        if acc.chars().count() >= MAX {
                            chunks.push(std::mem::take(&mut acc));
                        }
                    }
                    if !acc.is_empty() {
                        chunks.push(acc);
                    }
                } else if line_buf.chars().count() + line.chars().count() + 1 > MAX
                    && !line_buf.is_empty()
                {
                    chunks.push(std::mem::take(&mut line_buf));
                    line_buf.push_str(line);
                } else {
                    if !line_buf.is_empty() {
                        line_buf.push('\n');
                    }
                    line_buf.push_str(line);
                }
            }
            if !line_buf.is_empty() {
                chunks.push(line_buf);
            }
        } else {
            let added = if buf.is_empty() {
                para.chars().count()
            } else {
                buf.chars().count() + 2 + para.chars().count()
            };
            if added > MAX && !buf.is_empty() {
                chunks.push(std::mem::take(&mut buf));
                buf.push_str(para);
            } else {
                if !buf.is_empty() {
                    buf.push_str("\n\n");
                }
                buf.push_str(para);
            }
        }
    }
    if !buf.is_empty() {
        chunks.push(buf);
    }
    if chunks.is_empty() {
        chunks.push(String::new());
    }
    chunks
}

struct IngestReport {
    conversation_pk: i64,
    deduped: bool,
    message_count: usize,
    doc_count: usize,
}

fn ingest_chatgpt(
    conn: &mut Connection,
    bytes: &[u8],
    account_id: &str,
    workspace_id: &str,
    source: &str,
) -> Result<IngestReport, String> {
    let raw: Value =
        serde_json::from_slice(bytes).map_err(|err| format!("invalid conversation JSON: {err}"))?;
    let norm = normalize_chatgpt_json(&raw)?;
    let snapshot_hash = sha256_hex(bytes);
    let fetched_at = now_secs();
    let json_size = bytes.len() as i64;
    let message_count = norm.messages.len() as i64;
    let max_message_time = norm.max_message_time;

    let tx = conn.transaction().map_err(|err| err.to_string())?;

    let existing_pk: Option<(i64, Option<String>)> = tx
        .query_row(
            "SELECT conversation_pk, current_snapshot_hash FROM conversations
             WHERE account_id = ?1 AND workspace_id = ?2 AND remote_conversation_id = ?3",
            params![account_id, workspace_id, norm.remote_conversation_id],
            |row| Ok((row.get::<_, i64>(0)?, row.get::<_, Option<String>>(1)?)),
        )
        .optional()
        .map_err(|err| err.to_string())?;

    let conversation_pk = match existing_pk {
        Some((pk, current_hash)) => {
            if current_hash.as_deref() == Some(snapshot_hash.as_str()) {
                tx.execute(
                    "UPDATE conversations SET last_fetched_at = ?1 WHERE conversation_pk = ?2",
                    params![fetched_at, pk],
                )
                .map_err(|err| err.to_string())?;
                tx.commit().map_err(|err| err.to_string())?;
                return Ok(IngestReport {
                    conversation_pk: pk,
                    deduped: true,
                    message_count: norm.messages.len(),
                    doc_count: 0,
                });
            }
            tx.execute(
                "DELETE FROM messages WHERE conversation_pk = ?1",
                params![pk],
            )
            .map_err(|err| err.to_string())?;
            tx.execute(
                "DELETE FROM search_documents WHERE conversation_pk = ?1",
                params![pk],
            )
            .map_err(|err| err.to_string())?;
            pk
        }
        None => {
            tx.execute(
                "INSERT INTO conversations
                    (account_id, workspace_id, remote_conversation_id, title,
                     created_at_remote, updated_at_remote, last_message_at_remote,
                     last_fetched_at, last_indexed_at, current_snapshot_hash, freshness_state)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?8, ?9, 'fresh')",
                params![
                    account_id,
                    workspace_id,
                    norm.remote_conversation_id,
                    norm.title,
                    norm.created_at,
                    norm.updated_at,
                    norm.max_message_time,
                    fetched_at,
                    snapshot_hash,
                ],
            )
            .map_err(|err| err.to_string())?;
            tx.last_insert_rowid()
        }
    };

    tx.execute(
        "INSERT OR IGNORE INTO conversation_snapshots
            (snapshot_hash, conversation_pk, fetched_at, schema_version, source,
             json_blob, json_size_bytes, message_count, max_message_time)
         VALUES (?1, ?2, ?3, 1, ?4, ?5, ?6, ?7, ?8)",
        params![
            snapshot_hash,
            conversation_pk,
            fetched_at,
            source,
            String::from_utf8_lossy(bytes),
            json_size,
            message_count,
            max_message_time,
        ],
    )
    .map_err(|err| err.to_string())?;

    let mut doc_count = 0usize;
    for msg in &norm.messages {
        let text_hash = sha256_hex(msg.text.as_bytes());
        tx.execute(
            "INSERT INTO messages
                (conversation_pk, message_id, parent_message_id, role, content_type,
                 created_at_remote, updated_at_remote, text, text_hash, snapshot_hash, is_current)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, 1)",
            params![
                conversation_pk,
                msg.message_id,
                msg.parent_message_id,
                msg.role,
                msg.content_type,
                msg.created_at,
                msg.updated_at,
                msg.text,
                text_hash,
                snapshot_hash,
            ],
        )
        .map_err(|err| err.to_string())?;

        if msg.text.trim().is_empty() {
            continue;
        }
        let chunks = chunk_text(&msg.text);
        for (ord, chunk) in chunks.iter().enumerate() {
            tx.execute(
                "INSERT INTO search_documents
                    (conversation_pk, message_id, chunk_ord, title, text, text_ngram,
                     indexed_at, snapshot_hash)
                 VALUES (?1, ?2, ?3, ?4, ?5, lower(?5), ?6, ?7)",
                params![
                    conversation_pk,
                    msg.message_id,
                    ord as i64,
                    norm.title,
                    chunk,
                    fetched_at,
                    snapshot_hash,
                ],
            )
            .map_err(|err| err.to_string())?;
            doc_count += 1;
        }
    }

    tx.execute(
        "UPDATE conversations
            SET title = ?1, created_at_remote = ?2, updated_at_remote = ?3,
                last_message_at_remote = ?4, last_fetched_at = ?5, last_indexed_at = ?5,
                current_snapshot_hash = ?6, freshness_state = 'fresh'
         WHERE conversation_pk = ?7",
        params![
            norm.title,
            norm.created_at,
            norm.updated_at,
            norm.max_message_time,
            fetched_at,
            snapshot_hash,
            conversation_pk,
        ],
    )
    .map_err(|err| err.to_string())?;

    tx.commit().map_err(|err| err.to_string())?;
    Ok(IngestReport {
        conversation_pk,
        deduped: false,
        message_count: norm.messages.len(),
        doc_count,
    })
}

struct ChatgptHit {
    title: String,
    remote_conversation_id: String,
    message_id: String,
    role: String,
    snippet: String,
    created_at: Option<f64>,
    fetched_at: Option<f64>,
    freshness_state: String,
}

fn escape_like(s: &str) -> String {
    s.replace('\\', "\\\\")
        .replace('%', "\\%")
        .replace('_', "\\_")
}

fn cjk_char_count(s: &str) -> usize {
    s.chars()
        .filter(|c| {
            (*c >= '\u{4E00}' && *c <= '\u{9FFF}')
                || (*c >= '\u{3400}' && *c <= '\u{4DBF}')
                || (*c >= '\u{F900}' && *c <= '\u{FAFF}')
                || (*c >= '\u{3040}' && *c <= '\u{30FF}')
        })
        .count()
}

fn is_short_cjk_query(query: &str) -> bool {
    let n = cjk_char_count(query);
    n > 0 && n < 4
}

fn search_chatgpt(
    conn: &Connection,
    query: &str,
    limit: usize,
    account_id: Option<&str>,
    workspace_id: Option<&str>,
) -> Result<Vec<ChatgptHit>, String> {
    let q_lower = query.to_lowercase();
    let pattern = format!("%{}%", escape_like(&q_lower));
    let short_cjk = is_short_cjk_query(query);

    let mut sql = String::from(
        "SELECT sd.text, sd.title, m.message_id, m.role, m.created_at_remote,
                c.title, c.remote_conversation_id, c.conversation_pk,
                c.created_at_remote, c.last_fetched_at, c.freshness_state
         FROM search_documents sd
         JOIN conversations c ON c.conversation_pk = sd.conversation_pk
         LEFT JOIN messages m ON m.conversation_pk = sd.conversation_pk
              AND m.message_id = sd.message_id
         WHERE (lower(sd.text) LIKE ?1 ESCAPE '\\' OR lower(sd.title) LIKE ?1 ESCAPE '\\'
               OR lower(c.title) LIKE ?1 ESCAPE '\\')",
    );
    let mut params_vec: Vec<String> = vec![pattern.clone()];
    if let Some(a) = account_id {
        sql.push_str(" AND c.account_id = ?");
        params_vec.push(a.to_string());
    }
    if let Some(w) = workspace_id {
        sql.push_str(" AND c.workspace_id = ?");
        params_vec.push(w.to_string());
    }
    sql.push_str(" ORDER BY c.created_at_remote DESC, c.conversation_pk DESC");

    let mut stmt = conn.prepare(&sql).map_err(|err| err.to_string())?;
    let rows = stmt
        .query_map(rusqlite::params_from_iter(params_vec.iter()), |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, Option<String>>(2)?.unwrap_or_default(),
                row.get::<_, String>(3)?,
                row.get::<_, Option<f64>>(4)?,
                row.get::<_, String>(5)?,
                row.get::<_, String>(6)?,
                row.get::<_, i64>(7)?,
                row.get::<_, Option<f64>>(8)?,
                row.get::<_, Option<f64>>(9)?,
                row.get::<_, String>(10)?,
            ))
        })
        .map_err(|err| err.to_string())?;

    // Verify matches (especially short CJK) and dedup by conversation_pk,
    // keeping the highest-scoring document per conversation.
    let mut best: std::collections::HashMap<i64, (i64, ChatgptHit)> =
        std::collections::HashMap::new();
    for row in rows {
        let (
            text,
            sd_title,
            message_id,
            role,
            msg_created,
            conv_title,
            conv_id,
            pk,
            conv_created,
            fetched,
            freshness,
        ) = row.map_err(|err| err.to_string())?;

        let text_lower = text.to_lowercase();
        let title_lower = conv_title.to_lowercase();
        let matched_text = text_lower.contains(&q_lower);
        let matched_title = title_lower.contains(&q_lower);
        if short_cjk {
            // short CJK: require exact substring verification in the canonical text.
            if !text.contains(query) && !conv_title.contains(query) {
                continue;
            }
        } else if !(matched_text || matched_title) {
            continue;
        }

        let snippet = make_snippet(&text, query, &q_lower);
        let title = if conv_title.is_empty() {
            sd_title.clone()
        } else {
            conv_title.clone()
        };
        let recency = conv_created.unwrap_or(0.0) as i64;
        let score = if matched_title { 2_000_000 } else { 0 } + recency.max(0);
        best.entry(pk)
            .and_modify(|e| {
                if score > e.0 {
                    e.0 = score;
                    e.1 = ChatgptHit {
                        title: title.clone(),
                        remote_conversation_id: conv_id.clone(),
                        message_id: message_id.clone(),
                        role: role.clone(),
                        snippet: snippet.clone(),
                        created_at: msg_created,
                        fetched_at: fetched,
                        freshness_state: freshness.clone(),
                    };
                }
            })
            .or_insert_with(|| {
                (
                    score,
                    ChatgptHit {
                        title,
                        remote_conversation_id: conv_id,
                        message_id,
                        role,
                        snippet,
                        created_at: msg_created,
                        fetched_at: fetched,
                        freshness_state: freshness,
                    },
                )
            });
    }

    let mut hits: Vec<ChatgptHit> = best.into_values().map(|(_, h)| h).collect();
    hits.sort_by(|a, b| {
        let am = a.snippet.contains(query) as i64;
        let bm = b.snippet.contains(query) as i64;
        bm.cmp(&am).then_with(|| {
            b.created_at
                .unwrap_or(0.0)
                .partial_cmp(&a.created_at.unwrap_or(0.0))
                .unwrap_or(std::cmp::Ordering::Equal)
        })
    });
    hits.truncate(limit);
    Ok(hits)
}

fn make_snippet(text: &str, query: &str, query_lower: &str) -> String {
    let text_lower = text.to_lowercase();
    let byte_idx = text_lower.find(query_lower).or_else(|| text.find(query));
    let Some(byte_idx) = byte_idx else {
        return clip(text, 160);
    };
    let prefix_chars = text_lower[..byte_idx].chars().count();
    let total_chars = text.chars().count();
    let window = 80usize;
    let start = prefix_chars.saturating_sub(window);
    let end = (prefix_chars + query.chars().count() + window).min(total_chars);
    let chars: Vec<char> = text.chars().collect();
    let mut snip: String = chars[start..end].iter().collect();
    if start > 0 {
        snip.insert(0, char::from_u32(0x2026).unwrap_or('…'));
    }
    if end < total_chars {
        snip.push('…');
    }
    snip.replace('\n', " ")
}

fn chatgpt_ingest(
    cfg: &Config,
    file: &Path,
    account_id: &str,
    workspace_id: &str,
    source: &str,
) -> Result<(), String> {
    let bytes = fs::read(file).map_err(|err| format!("cannot read {}: {err}", file.display()))?;
    let db = chatgpt_db_path(cfg);
    let mut conn = open_chatgpt_db(&db)?;
    let report = ingest_chatgpt(&mut conn, &bytes, account_id, workspace_id, source)?;
    println!(
        "{}  pk={}  messages={}  docs={}{}",
        if report.deduped {
            "deduped"
        } else {
            "ingested"
        },
        report.conversation_pk,
        report.message_count,
        report.doc_count,
        if report.deduped {
            "  (snapshot unchanged)"
        } else {
            ""
        }
    );
    Ok(())
}

fn chatgpt_search(
    cfg: &Config,
    query: &str,
    limit: usize,
    account_id: Option<&str>,
    workspace_id: Option<&str>,
) -> Result<(), String> {
    let db = chatgpt_db_path(cfg);
    let conn = open_chatgpt_db(&db)?;
    let hits = search_chatgpt(&conn, query, limit, account_id, workspace_id)?;
    if hits.is_empty() {
        println!("no matches for {query:?}");
        return Ok(());
    }
    for hit in &hits {
        println!(
            "{}  {:<19}  {}",
            hit.remote_conversation_id,
            format_fetched(hit.fetched_at),
            clip(&hit.title, 92)
        );
        println!(
            "  message: {} [{}]  freshness: {}",
            hit.message_id, hit.role, hit.freshness_state
        );
        println!("  {}", clip(&hit.snippet, 240));
        println!();
    }
    println!("{} match(es) for {query:?}", hits.len());
    Ok(())
}

fn chatgpt_doctor(cfg: &Config) -> Result<(), String> {
    let db = chatgpt_db_path(cfg);
    let conn = open_chatgpt_db(&db)?;
    println!("db: {}", db.display());

    let count = |sql: &str| -> i64 {
        conn.query_row(sql, [], |row| row.get::<_, i64>(0))
            .unwrap_or(0)
    };

    let conversations = count("SELECT COUNT(*) FROM conversations");
    let snapshots = count("SELECT COUNT(*) FROM conversation_snapshots");
    let messages = count("SELECT COUNT(*) FROM messages");
    let docs = count("SELECT COUNT(*) FROM search_documents");
    let tombstones = count("SELECT COUNT(*) FROM tombstones");
    let queue = count("SELECT COUNT(*) FROM refresh_queue");

    println!("conversations:    {conversations}");
    println!("snapshots:        {snapshots}");
    println!("messages:         {messages}");
    println!("search_documents: {docs}");
    println!("tombstones:       {tombstones}");
    println!("refresh_queue:    {queue}");

    let known_not_fetched =
        count("SELECT COUNT(*) FROM conversations WHERE current_snapshot_hash IS NULL");
    let queued_refreshes = count("SELECT COUNT(*) FROM refresh_queue");
    let active_leases = count("SELECT COUNT(*) FROM refresh_leases WHERE status = 'active'");
    println!("known_not_fetched: {known_not_fetched}");
    println!("queued_refreshes:  {queued_refreshes}");
    println!("active_leases:      {active_leases}");
    let adapter_seen: Option<String> = conn
        .query_row(
            "SELECT value FROM service_state WHERE key = 'last_adapter_seen_at'",
            [],
            |r| r.get(0),
        )
        .optional()
        .ok()
        .flatten();
    match adapter_seen {
        Some(ts) => println!("adapter_last_seen: {ts}"),
        None => println!("adapter_last_seen: -"),
    }

    let no_snapshot =
        count("SELECT COUNT(*) FROM conversations WHERE current_snapshot_hash IS NULL");
    let orphan_docs = count(
        "SELECT COUNT(*) FROM search_documents sd
         LEFT JOIN conversations c ON c.conversation_pk = sd.conversation_pk
         WHERE c.conversation_pk IS NULL",
    );
    let dangling_hash = count(
        "SELECT COUNT(*) FROM conversations c
         WHERE current_snapshot_hash IS NOT NULL
           AND NOT EXISTS (SELECT 1 FROM conversation_snapshots s
                            WHERE s.conversation_pk = c.conversation_pk
                              AND s.snapshot_hash = c.current_snapshot_hash)",
    );
    let stale_messages = count(
        "SELECT COUNT(*) FROM messages m
         JOIN conversations c ON c.conversation_pk = m.conversation_pk
         WHERE m.snapshot_hash IS NOT c.current_snapshot_hash",
    );

    let mut problems = 0;
    if no_snapshot > 0 {
        println!("  WARN: {no_snapshot} conversation(s) with no current snapshot");
        problems += 1;
    }
    if orphan_docs > 0 {
        println!("  WARN: {orphan_docs} orphan search_document(s)");
        problems += 1;
    }
    if dangling_hash > 0 {
        println!("  WARN: {dangling_hash} conversation(s) with dangling snapshot hash");
        problems += 1;
    }
    if stale_messages > 0 {
        println!("  WARN: {stale_messages} message(s) not matching current snapshot");
        problems += 1;
    }
    if problems == 0 {
        println!("chatgpt-doctor: ok");
    } else {
        println!("chatgpt-doctor: {problems} problem(s)");
    }
    Ok(())
}

fn format_fetched(ts: Option<f64>) -> String {
    match ts {
        Some(t) if t > 0.0 => format!("fetched@{}", t as i64),
        _ => "—".to_string(),
    }
}

use std::net::TcpListener;
use std::thread;

const ALLOWED_ORIGIN: &str = "https://chatgpt.com";

/// Ingest request fields parsed from the HTTP body.
struct IngestRequest {
    payload: Value,
    account_id: String,
    workspace_id: String,
    source: String,
    #[allow(dead_code)]
    url: String,
    #[allow(dead_code)]
    route: String,
}

/// Parse the POST `/ingest/chatgpt/conversation` body into structured fields.
/// `payload` must be present (object or otherwise); `account_id`/`workspace_id`
/// default to "default", `source` defaults to "userscript:capture".
fn parse_ingest_body(body: &[u8]) -> Result<IngestRequest, String> {
    let v: Value =
        serde_json::from_slice(body).map_err(|err| format!("invalid JSON body: {err}"))?;
    let obj = v
        .as_object()
        .ok_or_else(|| "ingest body must be a JSON object".to_string())?;
    if !obj.contains_key("payload") {
        return Err("missing required field: payload".to_string());
    }
    let payload = obj.get("payload").cloned().unwrap_or(Value::Null);
    let account_id = obj
        .get("account_id")
        .and_then(|x| x.as_str())
        .unwrap_or("default")
        .to_string();
    let workspace_id = obj
        .get("workspace_id")
        .and_then(|x| x.as_str())
        .unwrap_or("default")
        .to_string();
    let source = obj
        .get("source")
        .and_then(|x| x.as_str())
        .unwrap_or("userscript:capture")
        .to_string();
    let url = obj
        .get("url")
        .and_then(|x| x.as_str())
        .unwrap_or("")
        .to_string();
    let route = obj
        .get("route")
        .and_then(|x| x.as_str())
        .unwrap_or("")
        .to_string();
    Ok(IngestRequest {
        payload,
        account_id,
        workspace_id,
        source,
        url,
        route,
    })
}

/// Route matcher for ChatGPT conversation detail endpoints.
///
/// Origin must equal `https://chatgpt.com` and the pathname must match
/// `^/backend-api/conversation/[^/?#]+/?$`. Query string and fragment are
/// ignored. Rejects `/textdocs`, `/files`, `/stream_status`, analytics, and
/// third-party origins.
fn route_matches_chatgpt_conversation(raw_url: &str) -> bool {
    let scheme_end = match raw_url.find("://") {
        Some(i) => i,
        None => return false,
    };
    let after = &raw_url[scheme_end + 3..];
    let auth_end = after.find(['/', '?', '#']).unwrap_or(after.len());
    let authority = &after[..auth_end];
    let origin = format!("{}://{}", &raw_url[..scheme_end], authority);
    if origin != ALLOWED_ORIGIN {
        return false;
    }
    let rest = &after[auth_end..];
    let path_end = rest.find(['?', '#']).unwrap_or(rest.len());
    let path = &rest[..path_end];
    chatgpt_conversation_path_matches(path)
}

/// Pure path component check (no scheme/host), used by tests.
fn chatgpt_conversation_path_matches(path: &str) -> bool {
    let prefix = "/backend-api/conversation/";
    if !path.starts_with(prefix) {
        return false;
    }
    let tail = &path[prefix.len()..];
    let (id_part, _trailing_slash) = match tail.strip_suffix('/') {
        Some(s) => (s, true),
        None => (tail, false),
    };
    if id_part.is_empty() {
        return false;
    }
    !id_part.chars().any(|c| c == '/' || c == '?' || c == '#')
}

/// Payload guard: payload must look like a ChatGPT conversation detail object.
fn payload_guard(payload: &Value) -> bool {
    let Some(obj) = payload.as_object() else {
        return false;
    };
    if !obj.contains_key("mapping") || !obj["mapping"].is_object() {
        return false;
    }
    let id_ok = obj.get("id").map(|v| v.is_string()).unwrap_or(false);
    let conv_id_ok = obj
        .get("conversation_id")
        .map(|v| v.is_string())
        .unwrap_or(false);
    id_ok || conv_id_ok
}

/// True when `Origin` is absent (non-browser local test) or exactly ChatGPT.
fn chatgpt_origin_ok(origin: Option<&str>) -> bool {
    match origin {
        None => true,
        Some(o) => o == ALLOWED_ORIGIN,
    }
}

/// Constant-time-ish token comparison to avoid short-circuit leakage.
fn token_matches(provided: Option<&str>, expected: &str) -> bool {
    let Some(provided) = provided else {
        return false;
    };
    let provided = provided.trim();
    let mut acc: u8 = 0;
    if provided.len() != expected.len() {
        acc = 1;
    }
    for (a, b) in provided.bytes().zip(expected.bytes()) {
        acc |= a ^ b;
    }
    acc == 0
}

/// Read a bearer token from the `Authorization` header value, if present.
/// Tokens in query strings are intentionally ignored.
fn bearer_token(header: Option<&str>) -> Option<String> {
    let header = header?;
    let trimmed = header.trim();
    let rest = trimmed.strip_prefix("Bearer ")?;
    Some(rest.trim().to_string())
}

/// Generate a 32-byte random hex token, preferring `/dev/urandom`.
fn generate_token() -> String {
    if let Ok(mut file) = fs::File::open("/dev/urandom") {
        let mut bytes = [0u8; 32];
        if file.read_exact(&mut bytes).is_ok() {
            let mut out = String::with_capacity(bytes.len() * 2);
            for b in &bytes {
                out.push_str(&format!("{b:02x}"));
            }
            return out;
        }
    }
    // Fallback: time + pid + counter, low quality but better than constant.
    let t = now_secs().to_bits();
    let pid = std::process::id();
    let counter = TOKEN_GEN_COUNTER.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    format!("{t:016x}{pid:08x}{counter:08x}")
}

static TOKEN_GEN_COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// Ensure a token file exists with an owner-only random token. Never returns
/// the token value; returns only the path the caller already knows.
fn ensure_token_file(path: &Path) -> Result<(), String> {
    if path.exists() {
        return Ok(());
    }
    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            fs::create_dir_all(parent)
                .map_err(|err| format!("cannot create token dir {}: {err}", parent.display()))?;
        }
    }
    let token = generate_token();
    let tmp = path.with_extension("tmp");
    fs::write(&tmp, token.as_bytes())
        .map_err(|err| format!("cannot write token file {}: {err}", path.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&tmp, fs::Permissions::from_mode(0o600))
            .map_err(|err| format!("cannot chmod token file: {err}"))?;
    }
    fs::rename(&tmp, path)
        .map_err(|err| format!("cannot rename token file {}: {err}", path.display()))?;
    Ok(())
}

fn read_token_file(path: &Path) -> Result<String, String> {
    let bytes = fs::read(path)
        .map_err(|err| format!("cannot read token file {}: {err}", path.display()))?;
    let token = String::from_utf8(bytes)
        .map_err(|err| format!("token file is not UTF-8: {err}"))?
        .trim()
        .to_string();
    if token.is_empty() {
        return Err("token file is empty".to_string());
    }
    Ok(token)
}

/// HTTP request parsed from a raw connection.
struct HttpRequest {
    method: String,
    target: String,
    headers: Vec<(String, String)>,
    body: Vec<u8>,
}

impl HttpRequest {
    fn header(&self, name: &str) -> Option<&str> {
        let lower = name.to_ascii_lowercase();
        self.headers
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case(&lower))
            .map(|(_, v)| v.as_str())
    }
}

/// Read a complete HTTP/1.1 request (request line, headers, Content-Length body).
fn read_request<R: Read>(reader: &mut R) -> Result<HttpRequest, String> {
    let mut buf: Vec<u8> = Vec::with_capacity(2048);
    let mut byte = [0u8; 1];
    loop {
        let n = reader
            .read(&mut byte)
            .map_err(|err| format!("read request: {err}"))?;
        if n == 0 {
            if buf.is_empty() {
                return Err("empty request".to_string());
            }
            break;
        }
        buf.push(byte[0]);
        if buf.ends_with(b"\r\n\r\n") {
            break;
        }
        if buf.len() > 65536 {
            return Err("request headers too large".to_string());
        }
    }
    let head = String::from_utf8(buf)
        .map_err(|err| format!("request not UTF-8: {err}"))?
        .trim_end_matches("\r\n")
        .to_string();
    let mut lines = head.split("\r\n");
    let request_line = lines
        .next()
        .ok_or_else(|| "missing request line".to_string())?;
    let parts: Vec<&str> = request_line.splitn(3, ' ').collect();
    if parts.len() != 3 {
        return Err("malformed request line".to_string());
    }
    let method = parts[0].to_string();
    let target = parts[1].to_string();
    let mut headers: Vec<(String, String)> = Vec::new();
    for line in lines {
        if line.is_empty() {
            continue;
        }
        if let Some((k, v)) = line.split_once(':') {
            headers.push((k.trim().to_string(), v.trim().to_string()));
        }
    }
    let content_length: usize = header_value(&headers, "content-length")
        .map(|v| v.parse().unwrap_or(0))
        .unwrap_or(0);
    let mut body = vec![0u8; content_length];
    if content_length > 0 {
        reader
            .read_exact(&mut body)
            .map_err(|err| format!("read body: {err}"))?;
    }
    Ok(HttpRequest {
        method,
        target,
        headers,
        body,
    })
}

fn header_value(headers: &[(String, String)], name: &str) -> Option<String> {
    headers
        .iter()
        .find(|(k, _)| k.eq_ignore_ascii_case(name))
        .map(|(_, v)| v.clone())
}

fn write_response<W: Write>(
    writer: &mut W,
    status: u16,
    reason: &str,
    cors: bool,
    body: &str,
) -> Result<(), String> {
    let mut resp = format!("HTTP/1.1 {status} {reason}\r\n");
    let mut extra = String::new();
    extra.push_str("Content-Type: application/json\r\n");
    extra.push_str(&format!("Content-Length: {}\r\n", body.len()));
    extra.push_str("Connection: close\r\n");
    if cors {
        extra.push_str("Access-Control-Allow-Origin: https://chatgpt.com\r\n");
        extra.push_str("Vary: Origin\r\n");
    }
    if status == 204 {
        // 204 has no body, but we keep a zero-length body for simplicity.
    }
    resp.push_str(&extra);
    resp.push_str("\r\n");
    writer
        .write_all(resp.as_bytes())
        .map_err(|err| format!("write response: {err}"))?;
    if !body.is_empty() {
        writer
            .write_all(body.as_bytes())
            .map_err(|err| format!("write body: {err}"))?;
    }
    writer.flush().ok();
    Ok(())
}

fn cors_preflight_response<W: Write>(
    writer: &mut W,
    allowed: bool,
    methods: &str,
) -> Result<(), String> {
    let (status, reason) = if allowed {
        (204, "No Content")
    } else {
        (403, "Forbidden")
    };
    let mut resp = format!("HTTP/1.1 {status} {reason}\r\n");
    if allowed {
        resp.push_str("Access-Control-Allow-Origin: https://chatgpt.com\r\n");
        resp.push_str("Access-Control-Allow-Headers: Authorization, Content-Type\r\n");
        resp.push_str(&format!("Access-Control-Allow-Methods: {methods}\r\n"));
        resp.push_str("Vary: Origin\r\n");
    }
    resp.push_str("Content-Length: 0\r\n");
    resp.push_str("Connection: close\r\n\r\n");
    writer
        .write_all(resp.as_bytes())
        .map_err(|err| format!("write preflight: {err}"))?;
    writer.flush().ok();
    Ok(())
}

fn handle_health<W: Write>(writer: &mut W) -> Result<(), String> {
    write_response(writer, 200, "OK", false, "{\"ok\":true}")
}

fn handle_ingest(
    req: &HttpRequest,
    db_path: &Path,
    token: &str,
) -> Result<(bool, String), (u16, String)> {
    let origin = req.header("origin");
    if !chatgpt_origin_ok(origin) {
        return Err((403, "{\"error\":\"forbidden origin\"}".to_string()));
    }
    let auth = req.header("authorization");
    let provided = bearer_token(auth);
    if !token_matches(provided.as_deref(), token) {
        return Err((401, "{\"error\":\"unauthorized\"}".to_string()));
    }
    let ingest =
        parse_ingest_body(&req.body).map_err(|err| (400, format!("{{\"error\":\"{err}\"}}")))?;
    if !payload_guard(&ingest.payload) {
        return Err((400, "{\"error\":\"payload guard failed\"}".to_string()));
    }
    if !ingest.url.is_empty() && !route_matches_chatgpt_conversation(&ingest.url) {
        return Err((400, "{\"error\":\"route guard failed\"}".to_string()));
    }
    if !ingest.route.is_empty()
        && ingest.route.starts_with("https://")
        && !route_matches_chatgpt_conversation(&ingest.route)
    {
        return Err((400, "{\"error\":\"route guard failed\"}".to_string()));
    }
    let payload_bytes = serde_json::to_vec(&ingest.payload)
        .map_err(|err| (400, format!("{{\"error\":\"payload reserialize: {err}\"}}")))?;
    let mut conn =
        open_chatgpt_db(db_path).map_err(|err| (500, format!("{{\"error\":\"{err}\"}}")))?;
    let report = ingest_chatgpt(
        &mut conn,
        &payload_bytes,
        &ingest.account_id,
        &ingest.workspace_id,
        &ingest.source,
    )
    .map_err(|err| (500, format!("{{\"error\":\"{err}\"}}")))?;
    let cors = origin == Some(ALLOWED_ORIGIN);
    let body = format!(
        "{{\"ok\":true,\"conversation_pk\":{},\"deduped\":{},\"message_count\":{},\"doc_count\":{}}}",
        report.conversation_pk,
        report.deduped,
        report.message_count,
        report.doc_count
    );
    Ok((cors, body))
}

/// Shared bearer-token + ChatGPT-origin check for browser-facing endpoints.
/// Returns `true` (cors) when the request carried the ChatGPT origin.
fn check_auth_and_origin(req: &HttpRequest, token: &str) -> Result<bool, (u16, String)> {
    let origin = req.header("origin");
    if !chatgpt_origin_ok(origin) {
        return Err((403, "{\"error\":\"forbidden origin\"}".to_string()));
    }
    let provided = bearer_token(req.header("authorization"));
    if !token_matches(provided.as_deref(), token) {
        return Err((401, "{\"error\":\"unauthorized\"}".to_string()));
    }
    Ok(origin == Some(ALLOWED_ORIGIN))
}

const LIST_INGEST_MAX_BYTES: usize = 256 * 1024;
const DIRTY_DEBOUNCE_SECS: f64 = 3.0;
const OPENED_PRIORITY: i64 = 100;
const DIRTY_PRIORITY: i64 = 80;
const LIST_DELTA_PRIORITY: i64 = 50;
const LEASE_DEADLINE_SECS: f64 = 30.0;
const LEASE_DEADLINE_MS: i64 = 30000;
const LEASE_POLL_AFTER_MS: i64 = 5000;
const MAX_CONSECUTIVE_FAILURES: i64 = 3;
const REPORT_FALLBACK_429_SECS: f64 = 60.0;
const AUTH_COOLDOWN_SECS: f64 = 300.0;
const NOT_FOUND_BACKOFF_SECS: f64 = 300.0;
const PER_CONV_FAILURE_BACKOFF_SECS: f64 = 60.0;

/// Fields the report endpoint must never accept (ChatGPT response bodies,
/// headers, or credentials must not be forwarded to the local service).
const FORBIDDEN_REPORT_FIELDS: &[&str] = &[
    "body",
    "response",
    "html",
    "json",
    "headers",
    "authorization",
    "cookie",
    "accessToken",
];

struct ExistingConv {
    pk: i64,
    last_fetched: Option<f64>,
    snapshot_hash: Option<String>,
    title: String,
    created_at: Option<f64>,
    updated_at: Option<f64>,
    visibility: String,
}

struct ListReport {
    seen: i64,
    upserted: i64,
    queued: i64,
}

/// Extract a scalar string field from a JSON value, ignoring non-strings.
fn scalar_str(v: &Value) -> Option<&str> {
    v.as_str()
}

/// Extract a scalar f64 field, accepting numbers or numeric strings.
fn scalar_f64(v: &Value) -> Option<f64> {
    as_f64(v)
}

/// Extract a scalar bool field, ignoring non-bools.
fn scalar_bool(v: &Value) -> Option<bool> {
    v.as_bool()
}

/// Upsert metadata-only conversation rows from a list ingest. Accepts only
/// scalar identity/scheduling fields; body-like fields are ignored. Does not
/// create snapshots, messages, or search_documents. Sets `last_seen_in_list_at`
/// and queues a detail refresh when the update marker is newer than
/// `last_fetched_at` or no snapshot exists.
fn ingest_chatgpt_list(
    conn: &mut Connection,
    items: &[Value],
    account_id: &str,
    workspace_id: &str,
) -> Result<ListReport, String> {
    let now = now_secs();
    let tx = conn.transaction().map_err(|e| e.to_string())?;
    let mut seen = 0i64;
    let mut upserted = 0i64;
    let mut queued = 0i64;
    for item in items {
        let item_obj = match item.as_object() {
            Some(o) => o,
            None => continue,
        };
        // Identity: prefer `id`, fall back to `conversation_id`. Only scalar
        // strings are accepted; nested objects/arrays are ignored.
        let id = item_obj
            .get("id")
            .and_then(scalar_str)
            .or_else(|| item_obj.get("conversation_id").and_then(scalar_str));
        let Some(id) = id else { continue };
        seen += 1;

        let title = item_obj.get("title").and_then(scalar_str).unwrap_or("");
        let create_time = item_obj.get("create_time").and_then(scalar_f64);
        let update_time = item_obj
            .get("update_time")
            .and_then(scalar_f64)
            .or_else(|| item_obj.get("updated_at").and_then(scalar_f64));
        let is_archived = item_obj
            .get("is_archived")
            .and_then(scalar_bool)
            .unwrap_or(false);
        let is_deleted = item_obj
            .get("is_deleted")
            .and_then(scalar_bool)
            .unwrap_or(false);
        let visibility = if is_deleted {
            "deleted"
        } else if is_archived {
            "archived"
        } else {
            "unknown"
        };

        // Body-like fields (mapping, messages, content, parts, text,
        // attachments, nested objects/arrays) are intentionally never read.

        let existing = tx
            .query_row(
                "SELECT conversation_pk, last_fetched_at, current_snapshot_hash,
                        title, created_at_remote, updated_at_remote, visibility_state
                 FROM conversations
                 WHERE account_id = ?1 AND workspace_id = ?2 AND remote_conversation_id = ?3",
                params![account_id, workspace_id, id],
                |row| {
                    Ok(ExistingConv {
                        pk: row.get(0)?,
                        last_fetched: row.get(1)?,
                        snapshot_hash: row.get(2)?,
                        title: row.get(3)?,
                        created_at: row.get(4)?,
                        updated_at: row.get(5)?,
                        visibility: row.get(6)?,
                    })
                },
            )
            .optional()
            .map_err(|e| e.to_string())?;

        let pk = match existing {
            Some(ExistingConv {
                pk,
                last_fetched,
                snapshot_hash,
                title: old_title,
                created_at: old_created,
                updated_at: old_updated,
                visibility: old_vis,
            }) => {
                let changed = old_title != title
                    || old_created != create_time
                    || old_updated != update_time
                    || old_vis != visibility;
                if changed {
                    tx.execute(
                        "UPDATE conversations
                            SET title = ?1,
                                created_at_remote = COALESCE(?2, created_at_remote),
                                updated_at_remote = COALESCE(?3, updated_at_remote),
                                last_seen_in_list_at = ?4,
                                visibility_state = ?5
                         WHERE conversation_pk = ?6",
                        params![title, create_time, update_time, now, visibility, pk],
                    )
                    .map_err(|e| e.to_string())?;
                    upserted += 1;
                }
                let should_queue = snapshot_hash.is_none()
                    || match (update_time, last_fetched) {
                        (Some(u), Some(lf)) => u > lf,
                        (Some(_), None) => true,
                        (None, _) => snapshot_hash.is_none(),
                    };
                if should_queue {
                    let res = tx
                        .execute(
                            "INSERT INTO refresh_queue (conversation_pk, reason, priority, not_before, attempt_count)
                             VALUES (?1, 'list_delta', ?2, ?3, 0)
                             ON CONFLICT(conversation_pk, reason) DO UPDATE SET
                                not_before = MIN(refresh_queue.not_before, excluded.not_before),
                                priority = MAX(refresh_queue.priority, excluded.priority)",
                            params![pk, LIST_DELTA_PRIORITY, now],
                        )
                        .map_err(|e| e.to_string())?;
                    if res > 0 {
                        queued += 1;
                    }
                }
                pk
            }
            None => {
                tx.execute(
                    "INSERT INTO conversations
                        (account_id, workspace_id, remote_conversation_id, title,
                         created_at_remote, updated_at_remote, last_seen_in_list_at,
                         freshness_state, visibility_state)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, 'unknown', ?8)",
                    params![
                        account_id,
                        workspace_id,
                        id,
                        title,
                        create_time,
                        update_time,
                        now,
                        visibility
                    ],
                )
                .map_err(|e| e.to_string())?;
                let pk = tx.last_insert_rowid();
                upserted += 1;
                // New row: no snapshot, so queue a detail refresh.
                let res = tx
                    .execute(
                        "INSERT OR IGNORE INTO refresh_queue
                            (conversation_pk, reason, priority, not_before, attempt_count)
                         VALUES (?1, 'list_delta', ?2, ?3, 0)",
                        params![pk, LIST_DELTA_PRIORITY, now],
                    )
                    .map_err(|e| e.to_string())?;
                if res > 0 {
                    queued += 1;
                }
                pk
            }
        };
        let _ = pk;
    }
    tx.commit().map_err(|e| e.to_string())?;
    Ok(ListReport {
        seen,
        upserted,
        queued,
    })
}

/// Parse a conversation id from a SPA navigation URL `https://chatgpt.com/c/{id}`.
fn parse_navigation_conversation_id(url: &str) -> Option<String> {
    let scheme_end = url.find("://")?;
    let after = &url[scheme_end + 3..];
    let auth_end = after.find(['/', '?', '#']).unwrap_or(after.len());
    let authority = &after[..auth_end];
    let origin = format!("{}://{}", &url[..scheme_end], authority);
    if origin != ALLOWED_ORIGIN {
        return None;
    }
    let path_start = &after[auth_end..];
    let path = path_start.split(['?', '#'].as_ref()).next().unwrap_or("");
    let parts: Vec<&str> = path.split('/').filter(|s| !s.is_empty()).collect();
    if parts.len() >= 2 && parts[0] == "c" {
        return Some(parts[1].to_string());
    }
    None
}

/// Upsert a known conversation row (no snapshot) and return its primary key.
fn upsert_known_conversation(
    tx: &Connection,
    account_id: &str,
    workspace_id: &str,
    remote_id: &str,
    now: f64,
) -> Result<i64, String> {
    let pk: Option<i64> = tx
        .query_row(
            "SELECT conversation_pk FROM conversations
             WHERE account_id = ?1 AND workspace_id = ?2 AND remote_conversation_id = ?3",
            params![account_id, workspace_id, remote_id],
            |r| r.get(0),
        )
        .optional()
        .map_err(|e| e.to_string())?;
    match pk {
        Some(p) => {
            tx.execute(
                "UPDATE conversations SET last_seen_in_list_at = ?1 WHERE conversation_pk = ?2",
                params![now, p],
            )
            .map_err(|e| e.to_string())?;
            Ok(p)
        }
        None => {
            tx.execute(
                "INSERT INTO conversations
                    (account_id, workspace_id, remote_conversation_id, title,
                     last_seen_in_list_at, freshness_state, visibility_state)
                 VALUES (?1, ?2, ?3, '', ?4, 'unknown', 'unknown')",
                params![account_id, workspace_id, remote_id, now],
            )
            .map_err(|e| e.to_string())?;
            Ok(tx.last_insert_rowid())
        }
    }
}

/// Set a `service_state` key/value pair (upsert).
fn set_service_state(tx: &Connection, key: &str, value: &str) -> Result<(), String> {
    tx.execute(
        "INSERT INTO service_state (key, value) VALUES (?1, ?2)
         ON CONFLICT(key) DO UPDATE SET value = excluded.value",
        params![key, value],
    )
    .map_err(|e| e.to_string())?;
    Ok(())
}

/// Read a `service_state` value as a parsed f64 (0.0 if missing/unparseable).
fn get_service_state_f64(conn: &Connection, key: &str) -> Result<f64, String> {
    let v: Option<String> = conn
        .query_row(
            "SELECT value FROM service_state WHERE key = ?1",
            params![key],
            |r| r.get(0),
        )
        .optional()
        .map_err(|e| e.to_string())?;
    Ok(v.and_then(|s| s.parse::<f64>().ok()).unwrap_or(0.0))
}

/// Enqueue (or coalesce) a detail refresh queue row. When `max_not_before` is
/// true the latest `not_before` wins (debounce pushes later); otherwise the
/// earliest wins (more urgent).
fn enqueue_refresh(
    tx: &Connection,
    pk: i64,
    reason: &str,
    priority: i64,
    not_before: f64,
    max_not_before: bool,
) -> Result<(), String> {
    let nb_expr = if max_not_before {
        "MAX(refresh_queue.not_before, excluded.not_before)"
    } else {
        "MIN(refresh_queue.not_before, excluded.not_before)"
    };
    let sql = format!(
        "INSERT INTO refresh_queue (conversation_pk, reason, priority, not_before, attempt_count)
         VALUES (?1, ?2, ?3, ?4, 0)
         ON CONFLICT(conversation_pk, reason) DO UPDATE SET
            not_before = {nb_expr},
            priority = MAX(refresh_queue.priority, excluded.priority)"
    );
    tx.execute(&sql, params![pk, reason, priority, not_before])
        .map_err(|e| e.to_string())?;
    Ok(())
}

/// Process one ChatGPT browser event. No conversation body text is accepted.
fn handle_chatgpt_event(
    conn: &mut Connection,
    kind: &str,
    conversation_id: Option<&str>,
    url: &str,
    reason: &str,
    account_id: &str,
    workspace_id: &str,
) -> Result<(), String> {
    let now = now_secs();
    let tx = conn.transaction().map_err(|e| e.to_string())?;
    match kind {
        "adapter_hello" => {
            set_service_state(&tx, "last_adapter_seen_at", &now.to_string())?;
        }
        "navigation" => {
            let cid: String = conversation_id
                .map(|s| s.to_string())
                .or_else(|| parse_navigation_conversation_id(url))
                .ok_or_else(|| "navigation event missing conversation_id".to_string())?;
            let pk = upsert_known_conversation(&tx, account_id, workspace_id, &cid, now)?;
            enqueue_refresh(&tx, pk, "opened", OPENED_PRIORITY, now, false)?;
        }
        "dirty" => {
            let cid: String = conversation_id
                .map(|s| s.to_string())
                .or_else(|| parse_navigation_conversation_id(url))
                .ok_or_else(|| "dirty event missing conversation_id".to_string())?;
            let pk = upsert_known_conversation(&tx, account_id, workspace_id, &cid, now)?;
            let _ = reason;
            enqueue_refresh(
                &tx,
                pk,
                "dirty",
                DIRTY_PRIORITY,
                now + DIRTY_DEBOUNCE_SECS,
                true,
            )?;
        }
        "delete" | "archive" => {
            if let Some(cid) = conversation_id {
                let vis = if kind == "delete" {
                    "deleted"
                } else {
                    "archived"
                };
                tx.execute(
                    "UPDATE conversations SET visibility_state = ?1
                     WHERE account_id = ?2 AND workspace_id = ?3 AND remote_conversation_id = ?4",
                    params![vis, account_id, workspace_id, cid],
                )
                .map_err(|e| e.to_string())?;
            }
        }
        other => return Err(format!("unknown event kind: {other}")),
    }
    tx.commit().map_err(|e| e.to_string())?;
    Ok(())
}

/// Generate an unpredictable opaque lease id (16 random bytes hex).
fn generate_lease_id() -> String {
    if let Ok(mut file) = fs::File::open("/dev/urandom") {
        let mut bytes = [0u8; 16];
        if file.read_exact(&mut bytes).is_ok() {
            return bytes.iter().map(|b| format!("{b:02x}")).collect::<String>();
        }
    }
    let t = now_secs().to_bits();
    let pid = std::process::id();
    let counter = LEASE_ID_COUNTER.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    format!("{t:016x}{pid:08x}{counter:08x}")
}

static LEASE_ID_COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

struct LeaseGrant {
    lease_id: Option<String>,
    conversation_id: Option<String>,
    url: Option<String>,
    deadline_ms: i64,
    poll_after_ms: i64,
}

/// Grant at most one active detail lease globally. Returns `None` (no work)
/// when a global cooldown is active, an active lease exists, or no due
/// `refresh_queue` row is available. Per-conversation consecutive failures
/// >= 3 are skipped.
fn grant_detail_lease(conn: &mut Connection) -> Result<LeaseGrant, String> {
    let now = now_secs();
    let tx = conn.transaction().map_err(|e| e.to_string())?;

    let cooldown = get_service_state_f64(&tx, "global_cooldown_until")?;
    if cooldown > now {
        let ms = (((cooldown - now) * 1000.0) as i64) + 1000;
        return Ok(LeaseGrant {
            lease_id: None,
            conversation_id: None,
            url: None,
            deadline_ms: 0,
            poll_after_ms: ms.max(1000),
        });
    }

    // Expire stale active leases whose deadline has passed.
    tx.execute(
        "UPDATE refresh_leases SET status = 'expired'
         WHERE status = 'active' AND deadline_at < ?1",
        params![now],
    )
    .map_err(|e| e.to_string())?;

    let active: i64 = tx
        .query_row(
            "SELECT COUNT(*) FROM refresh_leases WHERE status = 'active'",
            [],
            |r| r.get(0),
        )
        .map_err(|e| e.to_string())?;
    if active > 0 {
        return Ok(LeaseGrant {
            lease_id: None,
            conversation_id: None,
            url: None,
            deadline_ms: 0,
            poll_after_ms: LEASE_POLL_AFTER_MS,
        });
    }

    let row: Option<(i64, String)> = tx
        .query_row(
            "SELECT q.conversation_pk, c.remote_conversation_id
             FROM refresh_queue q
             JOIN conversations c ON c.conversation_pk = q.conversation_pk
             WHERE (q.not_before IS NULL OR q.not_before <= ?1)
               AND c.consecutive_failures < ?2
             ORDER BY q.priority DESC, q.not_before ASC
             LIMIT 1",
            params![now, MAX_CONSECUTIVE_FAILURES],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()
        .map_err(|e| e.to_string())?;

    let Some((conv_pk, conv_id)) = row else {
        return Ok(LeaseGrant {
            lease_id: None,
            conversation_id: None,
            url: None,
            deadline_ms: 0,
            poll_after_ms: LEASE_POLL_AFTER_MS,
        });
    };

    let lease_id = generate_lease_id();
    let deadline = now + LEASE_DEADLINE_SECS;
    let url = format!("https://chatgpt.com/backend-api/conversation/{conv_id}");
    tx.execute(
        "INSERT INTO refresh_leases
            (lease_id, conversation_pk, lease_type, url, granted_at, deadline_at, status)
         VALUES (?1, ?2, 'detail', ?3, ?4, ?5, 'active')",
        params![lease_id, conv_pk, url, now, deadline],
    )
    .map_err(|e| e.to_string())?;
    tx.commit().map_err(|e| e.to_string())?;
    Ok(LeaseGrant {
        lease_id: Some(lease_id),
        conversation_id: Some(conv_id),
        url: Some(url),
        deadline_ms: LEASE_DEADLINE_MS,
        poll_after_ms: 0,
    })
}

/// Process a lease report (status metadata only). Never accepts ChatGPT
/// response bodies, headers, or credentials.
fn process_lease_report(
    conn: &mut Connection,
    lease_id: &str,
    ok: bool,
    status: Option<i64>,
    retry_after_ms: Option<i64>,
    error: &str,
) -> Result<(), String> {
    let now = now_secs();
    let tx = conn.transaction().map_err(|e| e.to_string())?;

    let lease: Option<(Option<i64>, String)> = tx
        .query_row(
            "SELECT conversation_pk, status FROM refresh_leases WHERE lease_id = ?1",
            params![lease_id],
            |r| Ok((r.get::<_, Option<i64>>(0)?, r.get::<_, String>(1)?)),
        )
        .optional()
        .map_err(|e| e.to_string())?;
    let Some((conv_pk_opt, lease_status)) = lease else {
        return Err("lease not found".to_string());
    };
    if lease_status != "active" {
        return Err("lease is not active".to_string());
    }
    let conv_pk = conv_pk_opt.ok_or_else(|| "lease has no conversation".to_string())?;

    if ok {
        tx.execute(
            "UPDATE refresh_leases SET status = 'succeeded', completed_at = ?1 WHERE lease_id = ?2",
            params![now, lease_id],
        )
        .map_err(|e| e.to_string())?;
        tx.execute(
            "UPDATE conversations SET consecutive_failures = 0 WHERE conversation_pk = ?1",
            params![conv_pk],
        )
        .map_err(|e| e.to_string())?;
        // A successful detail fetch satisfies all pending refresh reasons.
        tx.execute(
            "DELETE FROM refresh_queue WHERE conversation_pk = ?1",
            params![conv_pk],
        )
        .map_err(|e| e.to_string())?;
    } else {
        tx.execute(
            "UPDATE refresh_leases SET status = 'failed', completed_at = ?1, last_error = ?2
             WHERE lease_id = ?3",
            params![now, error, lease_id],
        )
        .map_err(|e| e.to_string())?;
        tx.execute(
            "UPDATE conversations SET consecutive_failures = consecutive_failures + 1,
                                       last_error = ?1
             WHERE conversation_pk = ?2",
            params![error, conv_pk],
        )
        .map_err(|e| e.to_string())?;
        let backoff = match status {
            Some(429) => {
                let cd = retry_after_ms
                    .map(|m| m as f64 / 1000.0)
                    .unwrap_or(REPORT_FALLBACK_429_SECS);
                set_service_state(&tx, "global_cooldown_until", &(now + cd).to_string())?;
                PER_CONV_FAILURE_BACKOFF_SECS
            }
            Some(401) | Some(403) => {
                set_service_state(
                    &tx,
                    "global_cooldown_until",
                    &(now + AUTH_COOLDOWN_SECS).to_string(),
                )?;
                AUTH_COOLDOWN_SECS
            }
            Some(404) => {
                tx.execute(
                    "UPDATE conversations SET visibility_state = 'inaccessible'
                     WHERE conversation_pk = ?1",
                    params![conv_pk],
                )
                .map_err(|e| e.to_string())?;
                NOT_FOUND_BACKOFF_SECS
            }
            _ => PER_CONV_FAILURE_BACKOFF_SECS,
        };
        // Push the conversation's queue rows past the backoff window so the
        // same conversation is not re-leased immediately.
        tx.execute(
            "UPDATE refresh_queue SET not_before = ?1 WHERE conversation_pk = ?2",
            params![now + backoff, conv_pk],
        )
        .map_err(|e| e.to_string())?;
    }
    tx.commit().map_err(|e| e.to_string())?;
    Ok(())
}

/// `POST /ingest/chatgpt/list` handler.
fn handle_list_ingest(
    req: &HttpRequest,
    db_path: &Path,
    token: &str,
) -> Result<(bool, String), (u16, String)> {
    let cors = check_auth_and_origin(req, token)?;
    if req.body.len() > LIST_INGEST_MAX_BYTES {
        return Err((413, "{\"error\":\"request body too large\"}".to_string()));
    }
    let v: Value = serde_json::from_slice(&req.body)
        .map_err(|e| (400, format!("{{\"error\":\"invalid JSON: {e}\"}}")))?;
    let obj = v.as_object().ok_or_else(|| {
        (
            400,
            "{\"error\":\"body must be a JSON object\"}".to_string(),
        )
    })?;
    let account_id = obj
        .get("account_id")
        .and_then(|x| x.as_str())
        .unwrap_or("default");
    let workspace_id = obj
        .get("workspace_id")
        .and_then(|x| x.as_str())
        .unwrap_or("default");
    let items = obj
        .get("items")
        .and_then(|x| x.as_array())
        .ok_or_else(|| (400, "{\"error\":\"missing items array\"}".to_string()))?;
    let mut conn = open_chatgpt_db(db_path).map_err(|e| (500, format!("{{\"error\":\"{e}\"}}")))?;
    let report = ingest_chatgpt_list(&mut conn, items, account_id, workspace_id)
        .map_err(|e| (500, format!("{{\"error\":\"{e}\"}}")))?;
    let body = format!(
        "{{\"ok\":true,\"seen\":{},\"upserted\":{},\"queued\":{}}}",
        report.seen, report.upserted, report.queued
    );
    Ok((cors, body))
}

/// `POST /events/chatgpt` handler.
fn handle_events(
    req: &HttpRequest,
    db_path: &Path,
    token: &str,
) -> Result<(bool, String), (u16, String)> {
    let cors = check_auth_and_origin(req, token)?;
    let v: Value = serde_json::from_slice(&req.body)
        .map_err(|e| (400, format!("{{\"error\":\"invalid JSON: {e}\"}}")))?;
    let obj = v.as_object().ok_or_else(|| {
        (
            400,
            "{\"error\":\"body must be a JSON object\"}".to_string(),
        )
    })?;
    let kind = obj
        .get("kind")
        .and_then(|x| x.as_str())
        .ok_or_else(|| (400, "{\"error\":\"missing kind\"}".to_string()))?;
    let account_id = obj
        .get("account_id")
        .and_then(|x| x.as_str())
        .unwrap_or("default");
    let workspace_id = obj
        .get("workspace_id")
        .and_then(|x| x.as_str())
        .unwrap_or("default");
    let conversation_id = obj.get("conversation_id").and_then(|x| x.as_str());
    let url = obj.get("url").and_then(|x| x.as_str()).unwrap_or("");
    let reason = obj.get("reason").and_then(|x| x.as_str()).unwrap_or("");
    let mut conn = open_chatgpt_db(db_path).map_err(|e| (500, format!("{{\"error\":\"{e}\"}}")))?;
    handle_chatgpt_event(
        &mut conn,
        kind,
        conversation_id,
        url,
        reason,
        account_id,
        workspace_id,
    )
    .map_err(|e| (400, format!("{{\"error\":\"{e}\"}}")))?;
    Ok((cors, "{\"ok\":true}".to_string()))
}

/// `GET /refresh/chatgpt/lease` handler.
fn handle_lease(
    req: &HttpRequest,
    db_path: &Path,
    token: &str,
) -> Result<(bool, String), (u16, String)> {
    let cors = check_auth_and_origin(req, token)?;
    let mut conn = open_chatgpt_db(db_path).map_err(|e| (500, format!("{{\"error\":\"{e}\"}}")))?;
    let grant =
        grant_detail_lease(&mut conn).map_err(|e| (500, format!("{{\"error\":\"{e}\"}}")))?;
    let body = match grant.lease_id {
        Some(id) => format!(
            "{{\"ok\":true,\"lease\":{{\"lease_id\":\"{}\",\"type\":\"detail\",\"conversation_id\":\"{}\",\"url\":\"{}\",\"deadline_ms\":{}}}}}",
            id,
            grant.conversation_id.unwrap_or_default(),
            grant.url.unwrap_or_default(),
            grant.deadline_ms
        ),
        None => format!(
            "{{\"ok\":true,\"lease\":null,\"poll_after_ms\":{}}}",
            grant.poll_after_ms
        ),
    };
    Ok((cors, body))
}

/// `POST /refresh/chatgpt/report` handler.
fn handle_report(
    req: &HttpRequest,
    db_path: &Path,
    token: &str,
) -> Result<(bool, String), (u16, String)> {
    let cors = check_auth_and_origin(req, token)?;
    let v: Value = serde_json::from_slice(&req.body)
        .map_err(|e| (400, format!("{{\"error\":\"invalid JSON: {e}\"}}")))?;
    let obj = v.as_object().ok_or_else(|| {
        (
            400,
            "{\"error\":\"body must be a JSON object\"}".to_string(),
        )
    })?;
    for f in FORBIDDEN_REPORT_FIELDS {
        if obj.contains_key(*f) {
            return Err((400, format!("{{\"error\":\"forbidden field: {f}\"}}")));
        }
    }
    let lease_id = obj
        .get("lease_id")
        .and_then(|x| x.as_str())
        .ok_or_else(|| (400, "{\"error\":\"missing lease_id\"}".to_string()))?;
    let ok = obj.get("ok").and_then(|x| x.as_bool()).unwrap_or(false);
    let status = obj.get("status").and_then(|x| x.as_i64());
    let retry_after_ms = obj.get("retry_after_ms").and_then(|x| x.as_i64());
    let error = obj.get("error").and_then(|x| x.as_str()).unwrap_or("");
    let mut conn = open_chatgpt_db(db_path).map_err(|e| (500, format!("{{\"error\":\"{e}\"}}")))?;
    process_lease_report(&mut conn, lease_id, ok, status, retry_after_ms, error)
        .map_err(|e| (400, format!("{{\"error\":\"{e}\"}}")))?;
    Ok((cors, "{\"ok\":true}".to_string()))
}

/// Run the chatgpt-serve loopback ingest service until interrupted.
fn chatgpt_serve(cfg: &Config, addr: &str, token_file: &Path) -> Result<(), String> {
    ensure_token_file(token_file)?;
    let token = read_token_file(token_file)?;
    let db_path = chatgpt_db_path(cfg);
    // Open once to ensure schema exists.
    {
        let _conn = open_chatgpt_db(&db_path)?;
    }
    let listener = TcpListener::bind(addr).map_err(|err| format!("bind {addr}: {err}"))?;
    let local_addr = listener
        .local_addr()
        .map(|a| a.to_string())
        .unwrap_or_else(|_| addr.to_string());
    println!("chatgpt-serve listening on {local_addr}");
    println!("token-file: {}", token_file.display());
    println!("database:   {}", db_path.display());
    println!("endpoints:  GET /health, OPTIONS /ingest/chatgpt/conversation, POST /ingest/chatgpt/conversation");
    for stream in listener.incoming() {
        let stream = match stream {
            Ok(s) => s,
            Err(err) => {
                eprintln!("chatgpt-serve: accept error: {err}");
                continue;
            }
        };
        let db_path = db_path.clone();
        let token = token.clone();
        thread::spawn(move || {
            if let Err(err) = serve_one(stream, &db_path, &token) {
                eprintln!("chatgpt-serve: handler error: {err}");
            }
        });
    }
    Ok(())
}

fn serve_one(stream: std::net::TcpStream, db_path: &Path, token: &str) -> Result<(), String> {
    stream
        .set_read_timeout(Some(std::time::Duration::from_secs(30)))
        .map_err(|err| format!("set timeout: {err}"))?;
    let mut reader = std::io::BufReader::new(stream.try_clone().map_err(|e| e.to_string())?);
    let req = read_request(&mut reader)?;
    let (path_no_query, _query) = match req.target.split_once('?') {
        Some((p, q)) => (p, Some(q)),
        None => (req.target.as_str(), None),
    };
    let path_no_query = path_no_query.split('#').next().unwrap_or(path_no_query);
    let mut writer = stream;
    let origin = req.header("origin");
    let origin_ok = chatgpt_origin_ok(origin);
    let cors_origin = origin == Some(ALLOWED_ORIGIN);
    match (req.method.as_str(), path_no_query) {
        ("GET", "/health") => handle_health(&mut writer),
        // Conversation detail ingest
        ("OPTIONS", "/ingest/chatgpt/conversation") => {
            cors_preflight_response(&mut writer, origin_ok, "POST, OPTIONS")
        }
        ("POST", "/ingest/chatgpt/conversation") => match handle_ingest(&req, db_path, token) {
            Ok((cors, body)) => write_response(&mut writer, 200, "OK", cors, &body),
            Err((status, body)) => {
                let reason = http_reason(status);
                write_response(&mut writer, status, reason, false, &body)
            }
        },
        // List metadata ingest
        ("OPTIONS", "/ingest/chatgpt/list") => {
            cors_preflight_response(&mut writer, origin_ok, "POST, OPTIONS")
        }
        ("POST", "/ingest/chatgpt/list") => match handle_list_ingest(&req, db_path, token) {
            Ok((cors, body)) => write_response(&mut writer, 200, "OK", cors, &body),
            Err((status, body)) => {
                let reason = http_reason(status);
                let cors = if status == 403 { cors_origin } else { false };
                write_response(&mut writer, status, reason, cors, &body)
            }
        },
        // Browser events
        ("OPTIONS", "/events/chatgpt") => {
            cors_preflight_response(&mut writer, origin_ok, "POST, OPTIONS")
        }
        ("POST", "/events/chatgpt") => match handle_events(&req, db_path, token) {
            Ok((cors, body)) => write_response(&mut writer, 200, "OK", cors, &body),
            Err((status, body)) => {
                let reason = http_reason(status);
                let cors = if status == 403 { cors_origin } else { false };
                write_response(&mut writer, status, reason, cors, &body)
            }
        },
        // Refresh lease
        ("OPTIONS", "/refresh/chatgpt/lease") => {
            cors_preflight_response(&mut writer, origin_ok, "GET, OPTIONS")
        }
        ("GET", "/refresh/chatgpt/lease") => match handle_lease(&req, db_path, token) {
            Ok((cors, body)) => write_response(&mut writer, 200, "OK", cors, &body),
            Err((status, body)) => {
                let reason = http_reason(status);
                let cors = if status == 403 { cors_origin } else { false };
                write_response(&mut writer, status, reason, cors, &body)
            }
        },
        // Refresh report
        ("OPTIONS", "/refresh/chatgpt/report") => {
            cors_preflight_response(&mut writer, origin_ok, "POST, OPTIONS")
        }
        ("POST", "/refresh/chatgpt/report") => match handle_report(&req, db_path, token) {
            Ok((cors, body)) => write_response(&mut writer, 200, "OK", cors, &body),
            Err((status, body)) => {
                let reason = http_reason(status);
                let cors = if status == 403 { cors_origin } else { false };
                write_response(&mut writer, status, reason, cors, &body)
            }
        },
        _ => write_response(
            &mut writer,
            404,
            "Not Found",
            false,
            "{\"error\":\"not found\"}",
        ),
    }
}

/// Map an HTTP status code to its reason phrase for error responses.
fn http_reason(status: u16) -> &'static str {
    match status {
        400 => "Bad Request",
        401 => "Unauthorized",
        403 => "Forbidden",
        404 => "Not Found",
        413 => "Payload Too Large",
        500 => "Internal Server Error",
        _ => "Error",
    }
}
/// Print or install the ChatGPT browser userscript (two pieces).
fn chatgpt_userscript(
    _cfg: &Config,
    action: UserscriptAction,
    server: &str,
    token_file: &Path,
    embed_token: bool,
) -> Result<(), String> {
    match action {
        UserscriptAction::Print => {
            if embed_token {
                // Sensitive mode: reject non-loopback server BEFORE reading the
                // token file, so a misconfigured server never touches the token.
                validate_loopback_server(server)?;
                eprintln!(
                    "chatgpt-userscript: SENSITIVE -- token embedded in USER_SCRIPT sender only; do not log or commit"
                );
                let token = read_token_file(token_file)?;
                let main = main_world_script();
                let sender = sender_script(server, Some(&token));
                println!("// ===== MAIN-world hook (no token) =====");
                println!("{main}");
                println!();
                println!("// ===== USER_SCRIPT-world sender (token embedded) =====");
                println!("{sender}");
            } else {
                let main = main_world_script();
                let sender = sender_script(server, None);
                println!("// ===== MAIN-world hook (no token) =====");
                println!("{main}");
                println!();
                println!(
                    "// ===== USER_SCRIPT-world sender (no token; set via --embed-token) ====="
                );
                println!("{sender}");
            }
            Ok(())
        }
        UserscriptAction::Install => {
            // Reject non-loopback server BEFORE reading the token file.
            validate_loopback_server(server)?;
            install_chatgpt_userscript(server, token_file)
        }
    }
}

/// Validate that `server` is a loopback HTTP URL suitable for token-bearing
/// install/embed modes.
///
/// Accepts `http://127.0.0.1:<port>`, `http://localhost:<port>`, and
/// `http://[::1]:<port>`. Rejects `https`, non-loopback hosts, userinfo,
/// fragments, and paths other than `/` or empty. This must run before any
/// token file is read.
fn validate_loopback_server(server: &str) -> Result<(), String> {
    let rest = server
        .strip_prefix("http://")
        .ok_or_else(|| format!("--server must use an http:// loopback URL: {server}"))?;
    if rest.contains('#') {
        return Err(format!("--server must not contain a fragment: {server}"));
    }
    let (authority, after) = match rest.find(['/', '?'].as_ref()) {
        Some(i) => (&rest[..i], &rest[i..]),
        None => (rest, ""),
    };
    if authority.contains('@') {
        return Err(format!("--server must not contain userinfo: {server}"));
    }
    let (hostname, port) = parse_authority_host_port(authority, server)?;
    let is_loopback = matches!(hostname, "127.0.0.1" | "localhost" | "::1");
    if !is_loopback {
        return Err(format!(
            "--server must bind to loopback (127.0.0.1, localhost, or [::1]): {server}"
        ));
    }
    if port.is_empty() || !port.chars().all(|c| c.is_ascii_digit()) {
        return Err(format!("--server port must be numeric: {server}"));
    }
    let path = after.split('?').next().unwrap_or("");
    if !path.is_empty() && path != "/" {
        return Err(format!(
            "--server must have no path (or only '/'): {server}"
        ));
    }
    Ok(())
}

/// Split an authority string into `(hostname, port)`, handling IPv6 literals
/// like `[::1]:3500`.
fn parse_authority_host_port<'a>(
    authority: &'a str,
    server: &'a str,
) -> Result<(&'a str, &'a str), String> {
    if let Some(stripped) = authority.strip_prefix('[') {
        let end = stripped
            .find(']')
            .ok_or_else(|| format!("--server has malformed IPv6 literal: {server}"))?;
        let hostname = &stripped[..end];
        let after_bracket = &stripped[end + 1..];
        let port = after_bracket
            .strip_prefix(':')
            .ok_or_else(|| format!("--server must include a port: {server}"))?;
        return Ok((hostname, port));
    }
    let (h, p) = authority
        .rsplit_once(':')
        .ok_or_else(|| format!("--server must include a port: {server}"))?;
    Ok((h, p))
}

/// Install the two ChatGPT user scripts via the bro MCP service running on
/// `http://127.0.0.1:3500/mcp`. The token-bearing registration payload travels
/// only over this direct HTTP connection; it never goes through a shell
/// command, argv, an environment variable, stdout, or stderr.
fn install_chatgpt_userscript(server: &str, token_file: &Path) -> Result<(), String> {
    let token = read_token_file(token_file)?;
    warn_token_file_perms(token_file);

    let main_code = main_world_code();
    let sender_code = sender_code(server, &token);
    let scripts = build_bro_registration_scripts(&main_code, &sender_code);
    let ids = [CHATGPT_MAIN_SCRIPT_ID, CHATGPT_SENDER_SCRIPT_ID];

    let bro_token = read_bro_bearer_token()?;
    let mut client = RealMcpClient::new(BRO_MCP_URL, &bro_token)
        .map_err(|e| format!("cannot reach bro MCP at {BRO_MCP_URL}: {e}"))?;
    install_userscripts_via_bro(&mut client, &scripts, &ids)?;

    println!(
        "chatgpt-userscript: installed {} scripts via bro MCP",
        scripts.len()
    );
    println!("  bro endpoint: {BRO_MCP_URL}");
    println!("  ids: {}", ids.join(", "));
    println!("  ingest server: {server}");
    println!("  token-file: {}", token_file.display());
    Ok(())
}

/// Warn (stderr, no token value) if the token file is group/other readable.
fn warn_token_file_perms(path: &Path) {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if let Ok(meta) = fs::metadata(path) {
            let mode = meta.permissions().mode();
            if mode & 0o077 != 0 {
                eprintln!(
                    "chatgpt-userscript: warning: token file {} is group/other readable",
                    path.display()
                );
            }
        }
    }
}

/// Read the bro bearer token from `~/.bro/settings.json` (`token` field).
fn read_bro_bearer_token() -> Result<String, String> {
    let path = expand_home(BRO_SETTINGS_PATH);
    let bytes = fs::read(&path)
        .map_err(|err| format!("cannot read bro settings {}: {err}", path.display()))?;
    let v: Value = serde_json::from_slice(&bytes)
        .map_err(|err| format!("bro settings {path:?} is not valid JSON: {err}"))?;
    let token = v
        .get("token")
        .and_then(|t| t.as_str())
        .ok_or_else(|| "bro settings missing 'token' field".to_string())?;
    if token.is_empty() {
        return Err("bro settings token is empty".to_string());
    }
    Ok(token.to_string())
}

/// Build the two bro user-script registration objects with stable IDs.
///
/// The MAIN-world hook never contains the token. The USER_SCRIPT sender
/// contains the token. Both share `runAt=document_start`,
/// `matches=["https://chatgpt.com/*"]`, and `allFrames=false`.
fn build_bro_registration_scripts(main_code: &str, sender_code: &str) -> Vec<Value> {
    vec![
        bro_script_object(CHATGPT_MAIN_SCRIPT_ID, "MAIN", main_code),
        bro_script_object(CHATGPT_SENDER_SCRIPT_ID, "USER_SCRIPT", sender_code),
    ]
}

/// One bro user-script registration object.
fn bro_script_object(id: &str, world: &str, code: &str) -> Value {
    serde_json::json!({
        "id": id,
        "matches": ["https://chatgpt.com/*"],
        "js": [{ "code": code }],
        "runAt": "document_start",
        "allFrames": false,
        "world": world
    })
}

/// Orchestrate the bro MCP install sequence: initialize, notify, unregister
/// the two stable IDs, register, then verify via `userscripts_list`.
fn install_userscripts_via_bro<C: McpClient>(
    client: &mut C,
    scripts: &[Value],
    ids: &[&str],
) -> Result<(), String> {
    client.initialize()?;
    // Unregister the two stable IDs first so registration does not collide on
    // an existing id. A not-found error here is non-fatal (nothing to remove).
    let _ = client.call_tool(
        "userscripts_unregister",
        &mcp_userscripts_unregister_args(ids),
    );
    client
        .call_tool(
            "userscripts_register",
            &mcp_userscripts_register_args(scripts),
        )
        .map_err(|e| format!("bro userscripts_register failed: {e}"))?;
    let listed = client
        .call_tool("userscripts_list", &mcp_userscripts_list_args(ids))
        .map_err(|e| format!("bro userscripts_list failed: {e}"))?;
    verify_bro_listed_ids(&listed, ids)
}

/// Verify both expected IDs appear in the bro `userscripts_list` result blob.
fn verify_bro_listed_ids(listed: &Value, ids: &[&str]) -> Result<(), String> {
    let blob = listed.to_string();
    for id in ids {
        if !blob.contains(id) {
            return Err(format!(
                "bro userscripts_list verification failed: script id {id} not present after install"
            ));
        }
    }
    Ok(())
}

/// MCP `initialize` request params.
fn mcp_initialize_params() -> Value {
    serde_json::json!({
        "protocolVersion": "2024-11-05",
        "capabilities": {},
        "clientInfo": { "name": "chat-memory", "version": env!("CARGO_PKG_VERSION") }
    })
}

/// `userscripts_unregister` tool arguments.
fn mcp_userscripts_unregister_args(ids: &[&str]) -> Value {
    serde_json::json!({ "ids": ids })
}

/// `userscripts_register` tool arguments. The scripts array already contains
/// the generated code (token only in the sender).
fn mcp_userscripts_register_args(scripts: &[Value]) -> Value {
    serde_json::json!({ "scripts": scripts })
}

/// `userscripts_list` tool arguments.
fn mcp_userscripts_list_args(ids: &[&str]) -> Value {
    serde_json::json!({ "ids": ids })
}

/// Parsed HTTP response headers (lowercased keys plus a `:status` pseudo-header).
type HttpHeaders = Vec<(String, String)>;

/// Minimal MCP client trait. The real implementation talks to bro over a
/// blocking TCP HTTP connection; tests inject a fake that never touches the
/// network and never uses `Command`/argv.
trait McpClient {
    fn initialize(&mut self) -> Result<Value, String>;
    fn call_tool(&mut self, name: &str, args: &Value) -> Result<Value, String>;
}

/// Blocking JSON-RPC 2.0 client for the bro MCP HTTP endpoint. No async
/// runtime, no shell, no argv. The token-bearing registration payload is sent
/// only as the HTTP request body over a loopback TCP socket.
struct RealMcpClient {
    host: String,
    port: u16,
    path: String,
    bearer: String,
    session_id: Option<String>,
    next_id: u64,
}

impl RealMcpClient {
    fn new(url: &str, bearer: &str) -> Result<Self, String> {
        let (host, port, path) = parse_http_url(url)?;
        Ok(RealMcpClient {
            host,
            port,
            path,
            bearer: bearer.to_string(),
            session_id: None,
            next_id: 1,
        })
    }

    fn next_id(&mut self) -> Value {
        let id = self.next_id;
        self.next_id += 1;
        serde_json::json!(id)
    }

    fn round_trip(&mut self, req: &Value) -> Result<Value, String> {
        let body = serde_json::to_string(req).map_err(|e| format!("encode request: {e}"))?;
        let (headers, body_bytes) = http_post_json(
            &self.host,
            self.port,
            &self.path,
            &self.bearer,
            self.session_id.as_deref(),
            &body,
        )?;
        if let Some(sid) = header_value(&headers, "mcp-session-id") {
            self.session_id = Some(sid);
        }
        parse_mcp_response(&headers, &body_bytes)
    }
}

impl McpClient for RealMcpClient {
    fn initialize(&mut self) -> Result<Value, String> {
        let req = serde_json::json!({
            "jsonrpc": "2.0",
            "id": self.next_id(),
            "method": "initialize",
            "params": mcp_initialize_params()
        });
        self.round_trip(&req)
    }

    fn call_tool(&mut self, name: &str, args: &Value) -> Result<Value, String> {
        let req = serde_json::json!({
            "jsonrpc": "2.0",
            "id": self.next_id(),
            "method": "tools/call",
            "params": { "name": name, "arguments": args }
        });
        self.round_trip(&req)
    }
}

/// Parse an `http://host:port/path` URL into its parts (loopback expected).
fn parse_http_url(url: &str) -> Result<(String, u16, String), String> {
    let rest = url
        .strip_prefix("http://")
        .ok_or_else(|| format!("bro URL must be http://: {url}"))?;
    let (authority, after) = match rest.find(['/', '?', '#'].as_ref()) {
        Some(i) => (&rest[..i], &rest[i..]),
        None => (rest, ""),
    };
    let path = after.split(['?', '#'].as_ref()).next().unwrap_or("");
    let (host, port) = parse_authority_host_port(authority, url)?;
    let port: u16 = port
        .parse()
        .map_err(|_| format!("bro URL has invalid port: {url}"))?;
    let path = if path.is_empty() {
        "/".to_string()
    } else {
        path.to_string()
    };
    Ok((host.to_string(), port, path))
}

/// Extract the HTTP status code from parsed response headers (stored as the
/// pseudo `:status` entry produced by `read_http_response`).
#[cfg(test)]
fn http_status(headers: &[(String, String)]) -> Result<u16, String> {
    let s = header_value(headers, ":status").ok_or_else(|| "no HTTP status line".to_string())?;
    s.parse::<u16>()
        .map_err(|_| format!("invalid HTTP status: {s}"))
}

/// POST a JSON body to a loopback HTTP endpoint and return the response
/// headers (lowercased keys, plus a `:status` pseudo-header) and body bytes.
fn http_post_json(
    host: &str,
    port: u16,
    path: &str,
    bearer: &str,
    session_id: Option<&str>,
    body: &str,
) -> Result<(HttpHeaders, Vec<u8>), String> {
    let addr = format!("{host}:{port}");
    let mut stream = std::net::TcpStream::connect_timeout(
        &addr
            .parse()
            .map_err(|e| format!("invalid bro address {addr}: {e}"))?,
        std::time::Duration::from_secs(5),
    )
    .map_err(|e| format!("cannot connect to bro at {addr}: {e}"))?;
    stream
        .set_read_timeout(Some(std::time::Duration::from_secs(30)))
        .map_err(|e| format!("set read timeout: {e}"))?;
    let mut request = format!("POST {path} HTTP/1.1\r\n");
    request.push_str(&format!("Host: {host}:{port}\r\n"));
    request.push_str("Content-Type: application/json\r\n");
    request.push_str("Accept: application/json, text/event-stream\r\n");
    request.push_str(&format!("Authorization: Bearer {bearer}\r\n"));
    request.push_str(&format!("Content-Length: {}\r\n", body.len()));
    if let Some(sid) = session_id {
        request.push_str(&format!("Mcp-Session-Id: {sid}\r\n"));
    }
    request.push_str("Connection: close\r\n\r\n");
    let mut payload = request.into_bytes();
    payload.extend_from_slice(body.as_bytes());
    stream
        .write_all(&payload)
        .map_err(|e| format!("write to bro: {e}"))?;
    stream.flush().ok();
    let raw = read_all_stream(&mut stream)?;
    parse_http_response(&raw)
}

/// Read all bytes from a stream until the peer closes the connection.
fn read_all_stream(stream: &mut std::net::TcpStream) -> Result<Vec<u8>, String> {
    use std::io::Read;
    let mut buf: Vec<u8> = Vec::new();
    let mut chunk = [0u8; 4096];
    loop {
        let n = stream
            .read(&mut chunk)
            .map_err(|e| format!("read from bro: {e}"))?;
        if n == 0 {
            break;
        }
        buf.extend_from_slice(&chunk[..n]);
        if buf.len() > 8 * 1024 * 1024 {
            return Err("bro response too large".to_string());
        }
    }
    Ok(buf)
}

/// Split a raw HTTP response into headers (lowercased keys plus `:status`) and
/// body bytes, decoding chunked transfer encoding if present.
fn parse_http_response(raw: &[u8]) -> Result<(HttpHeaders, Vec<u8>), String> {
    let split = raw
        .windows(4)
        .position(|w| w == b"\r\n\r\n")
        .ok_or_else(|| "bro response has no header/body separator".to_string())?;
    let head = std::str::from_utf8(&raw[..split])
        .map_err(|e| format!("bro response headers not UTF-8: {e}"))?;
    let body = &raw[split + 4..];
    let mut lines = head.split("\r\n");
    let status_line = lines
        .next()
        .ok_or_else(|| "bro response missing status line".to_string())?;
    let status = status_line
        .split_whitespace()
        .nth(1)
        .ok_or_else(|| "bro response missing status code".to_string())?
        .to_string();
    let mut headers: Vec<(String, String)> = Vec::new();
    headers.push((":status".to_string(), status));
    for line in lines {
        if let Some((k, v)) = line.split_once(": ") {
            headers.push((k.to_ascii_lowercase(), v.trim().to_string()));
        }
    }
    let body = if header_value(&headers, "transfer-encoding")
        .map(|t| t.eq_ignore_ascii_case("chunked"))
        .unwrap_or(false)
    {
        decode_chunked(body)?
    } else if let Some(len) = header_value(&headers, "content-length") {
        let len: usize = len
            .parse()
            .map_err(|_| format!("invalid Content-Length: {len}"))?;
        if body.len() < len {
            return Err("bro response body shorter than Content-Length".to_string());
        }
        body[..len].to_vec()
    } else {
        body.to_vec()
    };
    Ok((headers, body))
}

/// Decode HTTP/1.1 chunked transfer encoding.
fn decode_chunked(body: &[u8]) -> Result<Vec<u8>, String> {
    let mut out: Vec<u8> = Vec::new();
    let mut pos = 0;
    while pos < body.len() {
        let line_end = body[pos..]
            .windows(2)
            .position(|w| w == b"\r\n")
            .map(|i| pos + i)
            .ok_or_else(|| "chunk size line missing CRLF".to_string())?;
        let size_str = std::str::from_utf8(&body[pos..line_end])
            .map_err(|e| format!("chunk size not UTF-8: {e}"))?;
        let size_str = size_str.split(';').next().unwrap_or(size_str).trim();
        let size = usize::from_str_radix(size_str, 16)
            .map_err(|_| format!("invalid chunk size: {size_str}"))?;
        pos = line_end + 2;
        if size == 0 {
            break;
        }
        if pos + size > body.len() {
            return Err("chunk body truncated".to_string());
        }
        out.extend_from_slice(&body[pos..pos + size]);
        pos += size;
        if body[pos..].starts_with(b"\r\n") {
            pos += 2;
        } else {
            return Err("chunk missing trailing CRLF".to_string());
        }
    }
    Ok(out)
}

/// Parse a JSON-RPC 2.0 response from either a JSON body or an SSE stream.
fn parse_mcp_response(headers: &[(String, String)], body: &[u8]) -> Result<Value, String> {
    let ct = header_value(headers, "content-type").unwrap_or_default();
    let raw = if ct.contains("text/event-stream") {
        let text = std::str::from_utf8(body).map_err(|e| format!("bro SSE not UTF-8: {e}"))?;
        extract_sse_json(text)?
    } else {
        let text = std::str::from_utf8(body).map_err(|e| format!("bro body not UTF-8: {e}"))?;
        serde_json::from_str(text).map_err(|e| format!("bro response not valid JSON: {e}"))?
    };
    if let Some(err) = raw.get("error") {
        return Err(format!("bro MCP error: {err}"));
    }
    raw.get("result")
        .cloned()
        .ok_or_else(|| format!("bro MCP response missing result: {raw}"))
}

/// Extract the first JSON-RPC message from an SSE text body (`data:` lines).
fn extract_sse_json(text: &str) -> Result<Value, String> {
    let mut data_lines: Vec<&str> = Vec::new();
    for line in text.lines() {
        if let Some(d) = line.strip_prefix("data:") {
            data_lines.push(d.trim_start_matches(' '));
        } else if !data_lines.is_empty() && line.is_empty() {
            let joined = data_lines.join("\n").trim().to_string();
            if !joined.is_empty() {
                return serde_json::from_str(&joined)
                    .map_err(|e| format!("bro SSE data not valid JSON: {e}"));
            }
            data_lines.clear();
        }
    }
    let joined = data_lines.join("\n").trim().to_string();
    if !joined.is_empty() {
        return serde_json::from_str(&joined)
            .map_err(|e| format!("bro SSE data not valid JSON: {e}"));
    }
    Err("bro SSE stream had no JSON data event".to_string())
}

/// Escape a value for safe interpolation into a JavaScript double-quoted
/// string. Prevents token/server values from breaking or injecting into the
/// generated script.
fn js_string_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            '\u{2028}' => out.push_str("\\u2028"),
            '\u{2029}' => out.push_str("\\u2029"),
            _ => out.push(c),
        }
    }
    out
}

/// MAIN-world hook code (no UserScript metadata header). Hooks fetch and
/// XMLHttpRequest, applies route and payload guards, emits events. NEVER
/// contains the token.
fn main_world_code() -> String {
    r#"(function () {
  "use strict";
  var ALLOWED_ORIGIN = "https://chatgpt.com";
  var ROUTE_RE = /^\/backend-api\/conversation\/[^\/?#]+\/?$/;
  var LIST_RE = /^\/backend-api\/conversations\/?$/;
  function isConversationUrl(raw) {
    try {
      var u = new URL(raw);
      if (u.origin !== ALLOWED_ORIGIN) return false;
      return ROUTE_RE.test(u.pathname);
    } catch (e) { return false; }
  }
  function isListUrl(raw) {
    try {
      var u = new URL(raw);
      if (u.origin !== ALLOWED_ORIGIN) return false;
      return LIST_RE.test(u.pathname);
    } catch (e) { return false; }
  }
  function currentConversationId() {
    try {
      var m = location.pathname.match(/^\/c\/([^\/?#]+)/);
      return m ? m[1] : "";
    } catch (e) { return ""; }
  }
  function guardPayload(p) {
    return p && typeof p === "object" && !Array.isArray(p)
      && p.mapping && typeof p.mapping === "object"
      && (typeof p.id === "string" || typeof p.conversation_id === "string");
  }
  function emit(payload, url) {
    try {
      var detail = {
        payload: payload, url: url, route: url,
        source: "userscript:capture",
        account_id: "default", workspace_id: "default"
      };
      window.dispatchEvent(new CustomEvent("chat-memory-capture", { detail: detail }));
      window.postMessage({ type: "chat-memory-capture", detail: detail }, ALLOWED_ORIGIN);
    } catch (e) {}
  }
  function listItems(data) {
    if (Array.isArray(data)) return data;
    if (!data || typeof data !== "object") return [];
    if (Array.isArray(data.items)) return data.items;
    if (Array.isArray(data.conversations)) return data.conversations;
    if (Array.isArray(data.data)) return data.data;
    return [];
  }
  function scalar(v) {
    return (typeof v === "string" || typeof v === "number" || typeof v === "boolean") ? v : undefined;
  }
  function reduceList(data) {
    var out = [];
    var items = listItems(data);
    for (var i = 0; i < items.length && out.length < 50; i++) {
      var it = items[i];
      if (!it || typeof it !== "object" || Array.isArray(it)) continue;
      var id = scalar(it.id);
      if (id === undefined) id = scalar(it.conversation_id);
      if (typeof id !== "string") continue;
      var row = { id: id };
      ["conversation_id","title","create_time","update_time","updated_at","is_archived","is_deleted","account_id","workspace_id"].forEach(function (k) {
        var v = scalar(it[k]);
        if (v !== undefined) row[k] = v;
      });
      out.push(row);
    }
    return out;
  }
  function emitList(data, url) {
    try {
      var items = reduceList(data);
      if (!items.length) return;
      var detail = { url: url, items: items, source: "userscript:list-capture", account_id: "default", workspace_id: "default" };
      window.dispatchEvent(new CustomEvent("chat-memory-list", { detail: detail }));
      window.postMessage({ type: "chat-memory-list", detail: detail }, ALLOWED_ORIGIN);
    } catch (e) {}
  }
  function emitEvent(kind, extra) {
    try {
      var detail = extra || {};
      detail.kind = kind;
      if (!detail.url) detail.url = location.href;
      if (!detail.conversation_id) detail.conversation_id = currentConversationId();
      detail.account_id = detail.account_id || "default";
      detail.workspace_id = detail.workspace_id || "default";
      window.dispatchEvent(new CustomEvent("chat-memory-event", { detail: detail }));
      window.postMessage({ type: "chat-memory-event", detail: detail }, ALLOWED_ORIGIN);
    } catch (e) {}
  }
  function emitNavigation() {
    var id = currentConversationId();
    if (id) emitEvent("navigation", { conversation_id: id, reason: "opened" });
  }
  function maybeDirty(method, rawUrl, status) {
    try {
      if (!(status >= 200 && status < 300)) return;
      method = String(method || "GET").toUpperCase();
      if (method === "GET" || method === "HEAD" || method === "OPTIONS") return;
      var u = new URL(rawUrl, location.href);
      if (u.origin !== ALLOWED_ORIGIN) return;
      if (!/^\/backend-api\//.test(u.pathname)) return;
      var id = currentConversationId();
      if (id) emitEvent("dirty", { conversation_id: id, reason: "mutation_observed" });
    } catch (e) {}
  }
  function hookFetch() {
    var orig = window.fetch;
    if (!orig) return;
    window.fetch = function () {
      var input = arguments[0], init = arguments[1] || {};
      var rawUrl = (typeof input === "string") ? input : (input && input.url) || "";
      var method = init.method || (input && input.method) || "GET";
      var p = orig.apply(this, arguments);
      p.then(function (resp) {
        try {
          if (isConversationUrl(resp.url)) {
            resp.clone().json().then(function (data) {
              if (guardPayload(data)) emit(data, resp.url);
            }).catch(function () {});
          } else if (isListUrl(resp.url)) {
            resp.clone().json().then(function (data) {
              emitList(data, resp.url);
            }).catch(function () {});
          }
          maybeDirty(method, resp.url || rawUrl, resp.status);
        } catch (e) {}
      }).catch(function () {});
      return p;
    };
  }
  function hookXHR() {
    var XHR = window.XMLHttpRequest;
    if (!XHR) return;
    var oOpen = XHR.prototype.open, oSend = XHR.prototype.send;
    XHR.prototype.open = function (m, url) {
      this.__cm_url = url;
      this.__cm_method = m;
      return oOpen.apply(this, arguments);
    };
    XHR.prototype.send = function () {
      var self = this;
      this.addEventListener("load", function () {
        try {
          if (isConversationUrl(self.__cm_url)) {
            var data = JSON.parse(self.responseText);
            if (guardPayload(data)) emit(data, self.__cm_url);
          } else if (isListUrl(self.__cm_url)) {
            emitList(JSON.parse(self.responseText), self.__cm_url);
          }
          maybeDirty(self.__cm_method, self.__cm_url, self.status);
        } catch (e) {}
      });
      return oSend.apply(this, arguments);
    };
  }
  function hookNavigation() {
    var oPush = history.pushState, oReplace = history.replaceState;
    history.pushState = function () {
      var r = oPush.apply(this, arguments);
      setTimeout(emitNavigation, 0);
      return r;
    };
    history.replaceState = function () {
      var r = oReplace.apply(this, arguments);
      setTimeout(emitNavigation, 0);
      return r;
    };
    window.addEventListener("popstate", function () { setTimeout(emitNavigation, 0); });
    window.addEventListener("hashchange", function () { setTimeout(emitNavigation, 0); });
    setTimeout(function () {
      emitEvent("adapter_hello", {});
      emitNavigation();
    }, 0);
  }
  hookFetch();
  hookXHR();
  hookNavigation();
})();
"#
    .to_string()
}

/// MAIN-world hook script with a UserScript metadata header, for `print`.
fn main_world_script() -> String {
    let header = "// ==UserScript==\n// @name         chat-memory ChatGPT hook (MAIN world)\n// @match        https://chatgpt.com/*\n// @run-at       document-start\n// @grant        none\n// ==/UserScript==\n";
    format!("{header}{}", main_world_code())
}

/// USER_SCRIPT-world sender code (no UserScript metadata header). Holds the
/// token and POSTs guarded payloads to the loopback ingest service. It treats
/// every page event as untrusted: it repeats the route guard and payload guard
/// before POSTing, accepts only the known event schema, and forwards no page
/// headers, cookies, or authorization values.
fn sender_code(server: &str, token: &str) -> String {
    let server_esc = js_string_escape(server);
    let token_esc = js_string_escape(token);
    format!(
        r#"(function () {{
  "use strict";
  var SERVER = "{server_esc}";
  var TOKEN = "{token_esc}";
  var ALLOWED_ORIGIN = "https://chatgpt.com";
  var ROUTE_RE = /^\/backend-api\/conversation\/[^\/?#]+\/?$/;
  var LIST_RE = /^\/backend-api\/conversations\/?$/;
  var MAX_DETAIL_LEASES_PER_TAB = 20;
  var failures = 0;
  var stopped = false;
  var leaseBusy = false;
  var leaseCount = 0;
  function isConversationUrl(raw) {{
    try {{
      var u = new URL(raw);
      if (u.origin !== ALLOWED_ORIGIN) return false;
      return ROUTE_RE.test(u.pathname);
    }} catch (e) {{ return false; }}
  }}
  function isListUrl(raw) {{
    try {{
      var u = new URL(raw);
      if (u.origin !== ALLOWED_ORIGIN) return false;
      return LIST_RE.test(u.pathname);
    }} catch (e) {{ return false; }}
  }}
  function guardPayload(p) {{
    return p && typeof p === "object" && !Array.isArray(p)
      && p.mapping && typeof p.mapping === "object"
      && (typeof p.id === "string" || typeof p.conversation_id === "string");
  }}
  function scalar(v) {{
    return (typeof v === "string" || typeof v === "number" || typeof v === "boolean") ? v : undefined;
  }}
  function reduceListItems(items) {{
    var out = [];
    if (!Array.isArray(items)) return out;
    for (var i = 0; i < items.length && out.length < 50; i++) {{
      var it = items[i];
      if (!it || typeof it !== "object" || Array.isArray(it)) continue;
      var id = scalar(it.id);
      if (id === undefined) id = scalar(it.conversation_id);
      if (typeof id !== "string") continue;
      var row = {{ id: id }};
      ["conversation_id","title","create_time","update_time","updated_at","is_archived","is_deleted","account_id","workspace_id"].forEach(function (k) {{
        var v = scalar(it[k]);
        if (v !== undefined) row[k] = v;
      }});
      out.push(row);
    }}
    return out;
  }}
  function noteFailure(status) {{
    failures += 1;
    if (failures >= 8) {{
      if (!stopped) console.warn("[chat-memory] local ingest unavailable; backing off for page lifetime");
      stopped = true;
    }} else {{
      console.warn("[chat-memory] local ingest failed (" + status + "); attempt " + failures);
    }}
  }}
  function localFetch(path, opts) {{
    opts = opts || {{}};
    opts.mode = "cors";
    opts.headers = opts.headers || {{}};
    opts.headers["Authorization"] = "Bearer " + TOKEN;
    if (opts.body !== undefined) opts.headers["Content-Type"] = "application/json";
    return fetch(SERVER + path, opts);
  }}
  function postCapture(detail) {{
    if (stopped) return;
    if (!detail || typeof detail !== "object") return;
    // Trust boundary: page events are untrusted. Re-validate route and payload.
    var url = typeof detail.url === "string" ? detail.url
      : (typeof detail.route === "string" ? detail.route : "");
    if (!isConversationUrl(url)) return;
    var payload = detail.payload;
    if (!guardPayload(payload)) return;
    // Only the known schema fields are read; extra fields are ignored. No
    // page-controlled headers, cookies, or authorization values are forwarded.
    var body = {{
      account_id: typeof detail.account_id === "string" ? detail.account_id : "default",
      workspace_id: typeof detail.workspace_id === "string" ? detail.workspace_id : "default",
      source: typeof detail.source === "string" ? detail.source : "userscript:capture",
      url: url,
      route: url,
      payload: payload
    }};
    try {{
      localFetch("/ingest/chatgpt/conversation", {{
        method: "POST",
        body: JSON.stringify(body)
      }}).then(function (r) {{
        if (r.status >= 200 && r.status < 300) failures = 0; else noteFailure(r.status);
      }}).catch(function () {{ noteFailure(0); }});
    }} catch (e) {{ noteFailure(0); }}
  }}
  function postList(detail) {{
    if (stopped || !detail || typeof detail !== "object") return;
    var url = typeof detail.url === "string" ? detail.url : "";
    if (!isListUrl(url)) return;
    var items = reduceListItems(detail.items);
    if (!items.length) return;
    var body = {{
      account_id: typeof detail.account_id === "string" ? detail.account_id : "default",
      workspace_id: typeof detail.workspace_id === "string" ? detail.workspace_id : "default",
      source: "userscript:list-capture",
      items: items
    }};
    try {{
      localFetch("/ingest/chatgpt/list", {{ method: "POST", body: JSON.stringify(body) }})
        .then(function (r) {{ if (r.status >= 200 && r.status < 300) failures = 0; else noteFailure(r.status); }})
        .catch(function () {{ noteFailure(0); }});
    }} catch (e) {{ noteFailure(0); }}
  }}
  function postEvent(detail) {{
    if (stopped || !detail || typeof detail !== "object") return;
    var kind = typeof detail.kind === "string" ? detail.kind : "";
    if (["navigation","dirty","adapter_hello"].indexOf(kind) < 0) return;
    var body = {{
      kind: kind,
      account_id: typeof detail.account_id === "string" ? detail.account_id : "default",
      workspace_id: typeof detail.workspace_id === "string" ? detail.workspace_id : "default"
    }};
    if (typeof detail.conversation_id === "string") body.conversation_id = detail.conversation_id;
    if (typeof detail.url === "string") body.url = detail.url;
    if (typeof detail.reason === "string") body.reason = detail.reason;
    try {{
      localFetch("/events/chatgpt", {{ method: "POST", body: JSON.stringify(body) }})
        .then(function (r) {{ if (r.status >= 200 && r.status < 300) failures = 0; else noteFailure(r.status); }})
        .catch(function () {{ noteFailure(0); }});
    }} catch (e) {{ noteFailure(0); }}
  }}
  function retryAfterMs(resp) {{
    try {{
      var h = resp.headers.get("Retry-After");
      if (!h) return undefined;
      var n = Number(h);
      if (Number.isFinite(n)) return Math.max(0, Math.floor(n * 1000));
    }} catch (e) {{}}
    return undefined;
  }}
  function reportLease(leaseId, ok, status, error, retryMs) {{
    var body = {{ lease_id: leaseId, ok: !!ok }};
    if (typeof status === "number") body.status = status;
    if (typeof error === "string" && error) body.error = error.slice(0, 120);
    if (typeof retryMs === "number") body.retry_after_ms = retryMs;
    return localFetch("/refresh/chatgpt/report", {{ method: "POST", body: JSON.stringify(body) }})
      .then(function (r) {{ if (!(r.status >= 200 && r.status < 300)) noteFailure(r.status); }})
      .catch(function () {{ noteFailure(0); }});
  }}
  function runLease(lease) {{
    if (!lease || lease.type !== "detail" || typeof lease.lease_id !== "string" || !isConversationUrl(lease.url)) return;
    if (leaseCount >= MAX_DETAIL_LEASES_PER_TAB) return;
    leaseCount += 1;
    fetch(lease.url, {{ method: "GET", credentials: "include" }})
      .then(function (resp) {{
        if (!(resp.status >= 200 && resp.status < 300)) {{
          return reportLease(lease.lease_id, false, resp.status, "detail_fetch_failed", retryAfterMs(resp));
        }}
        return resp.clone().json().then(function (data) {{
          if (!guardPayload(data)) return reportLease(lease.lease_id, false, resp.status, "invalid_detail_payload");
          return new Promise(function (resolve) {{
            postCapture({{
              payload: data,
              url: lease.url,
              route: lease.url,
              source: "userscript:refresh",
              account_id: "default",
              workspace_id: "default"
            }});
            setTimeout(resolve, 0);
          }}).then(function () {{
            return reportLease(lease.lease_id, true, resp.status, "");
          }});
        }}).catch(function () {{
          return reportLease(lease.lease_id, false, resp.status, "detail_json_parse_failed");
        }});
      }})
      .catch(function () {{
        return reportLease(lease.lease_id, false, 0, "detail_fetch_network_error");
      }});
  }}
  function pollLease(delay) {{
    if (stopped) return;
    if (leaseBusy || leaseCount >= MAX_DETAIL_LEASES_PER_TAB) {{
      setTimeout(function () {{ pollLease(5000); }}, 5000);
      return;
    }}
    leaseBusy = true;
    localFetch("/refresh/chatgpt/lease", {{ method: "GET" }})
      .then(function (r) {{
        if (!(r.status >= 200 && r.status < 300)) {{
          noteFailure(r.status);
          return {{ poll_after_ms: 10000 }};
        }}
        failures = 0;
        return r.json();
      }})
      .then(function (data) {{
        var next = (data && typeof data.poll_after_ms === "number") ? data.poll_after_ms : 5000;
        if (data && data.lease) {{
          runLease(data.lease);
          next = 1000;
        }}
        leaseBusy = false;
        setTimeout(function () {{ pollLease(next); }}, Math.max(1000, next));
      }})
      .catch(function () {{
        leaseBusy = false;
        noteFailure(0);
        setTimeout(function () {{ pollLease(10000); }}, 10000);
      }});
  }}
  window.addEventListener("chat-memory-capture", function (e) {{
    if (!e || e.type !== "chat-memory-capture") return;
    postCapture(e.detail);
  }}, false);
  window.addEventListener("chat-memory-list", function (e) {{
    if (!e || e.type !== "chat-memory-list") return;
    postList(e.detail);
  }}, false);
  window.addEventListener("chat-memory-event", function (e) {{
    if (!e || e.type !== "chat-memory-event") return;
    postEvent(e.detail);
  }}, false);
  window.addEventListener("message", function (e) {{
    if (e.source !== window) return;
    if (e.data && e.data.type === "chat-memory-capture" && e.data.detail) postCapture(e.data.detail);
    if (e.data && e.data.type === "chat-memory-list" && e.data.detail) postList(e.data.detail);
    if (e.data && e.data.type === "chat-memory-event" && e.data.detail) postEvent(e.data.detail);
  }}, false);
  try {{ postEvent({{ kind: "adapter_hello" }}); }} catch (e) {{}}
  setTimeout(function () {{ pollLease(1000); }}, 1000);
}})();
"#
    )
}

/// USER_SCRIPT-world sender script with a UserScript metadata header, for
/// `print`. The token is embedded only when `token` is `Some`.
fn sender_script(server: &str, token: Option<&str>) -> String {
    let tok = token.unwrap_or("");
    let header = "// ==UserScript==\n// @name         chat-memory ChatGPT sender (USER_SCRIPT world)\n// @match        https://chatgpt.com/*\n// @run-at       document-start\n// @grant        none\n// ==/UserScript==\n";
    format!("{header}{}", sender_code(server, tok))
}
#[cfg(test)]
mod chatgpt_tests {
    use super::*;

    fn dummy_config() -> Config {
        Config {
            agent: "all".to_string(),
            cache: PathBuf::from("/tmp/chat-memory-dummy.sqlite3"),
            codex_home: PathBuf::from("/tmp/.codex"),
            opencode_db: PathBuf::from("/tmp/opencode.db"),
            command: CommandKind::Count,
        }
    }

    fn fixture_json() -> &'static str {
        r#"{
            "id": "conv-001",
            "title": "Book and movie chat",
            "create_time": 1700000000.0,
            "update_time": 1700001000.0,
            "mapping": {
                "root": {"id": "root"},
                "a": {
                    "id": "a",
                    "parent": "root",
                    "message": {
                        "id": "m1",
                        "author": {"role": "user"},
                        "content": {"content_type": "text", "parts": ["Have you seen the 电影 adaptation of Dune?"]},
                        "create_time": 1700000050.0
                    }
                },
                "b": {
                    "id": "b",
                    "parent": "a",
                    "message": {
                        "id": "m2",
                        "author": {"role": "assistant"},
                        "content": {"content_type": "text", "parts": ["I liked the 电影 version. You might also enjoy Project Hail Mary by Andy Weir."]},
                        "create_time": 1700000100.0
                    }
                }
            }
        }"#
    }

    static COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    fn temp_db() -> PathBuf {
        let n = COUNTER.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        let dir = env::temp_dir().join(format!("chatgpt-test-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        dir.join(format!("test-{n}.sqlite3"))
    }

    #[test]
    fn ingest_normalize_and_search_movie_cjk() {
        let db = temp_db();
        let mut conn = open_chatgpt_db(&db).unwrap();
        let bytes = fixture_json().as_bytes().to_vec();
        let report = ingest_chatgpt(&mut conn, &bytes, "acct", "ws", "test").unwrap();
        assert!(!report.deduped);
        assert_eq!(report.message_count, 2);
        assert_eq!(report.doc_count, 2);

        // Short CJK query 电影 must match via exact substring verification.
        let hits = search_chatgpt(&conn, "电影", 20, None, None).unwrap();
        assert_eq!(hits.len(), 1, "电影 should match one conversation");
        assert_eq!(hits[0].remote_conversation_id, "conv-001");
        assert!(hits[0].snippet.contains("电影"));

        // English phrase search.
        let hits = search_chatgpt(&conn, "Project Hail Mary", 20, None, None).unwrap();
        assert_eq!(hits.len(), 1);
        assert!(hits[0].snippet.contains("Project Hail Mary"));

        // Negative: non-existent term returns nothing.
        let hits = search_chatgpt(&conn, "nonexistentterm123", 20, None, None).unwrap();
        assert!(hits.is_empty());
    }

    #[test]
    fn ingest_dedup_on_same_snapshot() {
        let db = temp_db();
        let mut conn = open_chatgpt_db(&db).unwrap();
        let bytes = fixture_json().as_bytes().to_vec();
        let first = ingest_chatgpt(&mut conn, &bytes, "acct", "ws", "test").unwrap();
        assert!(!first.deduped);
        let second = ingest_chatgpt(&mut conn, &bytes, "acct", "ws", "test").unwrap();
        assert!(second.deduped);
        assert_eq!(first.conversation_pk, second.conversation_pk);
        let last_fetched_at: Option<f64> = conn
            .query_row(
                "SELECT last_fetched_at FROM conversations WHERE conversation_pk = ?1",
                params![second.conversation_pk],
                |row| row.get(0),
            )
            .unwrap();
        assert!(last_fetched_at.unwrap_or(0.0) > 0.0);
    }

    #[test]
    fn reindex_on_changed_snapshot() {
        let db = temp_db();
        let mut conn = open_chatgpt_db(&db).unwrap();
        ingest_chatgpt(&mut conn, fixture_json().as_bytes(), "acct", "ws", "test").unwrap();

        let updated = r#"{
            "id": "conv-001",
            "title": "Updated title",
            "create_time": 1700000000.0,
            "update_time": 1700002000.0,
            "mapping": {
                "a": {"id": "a", "message": {"id": "m1", "author": {"role": "user"},
                    "content": {"content_type": "text", "parts": ["Tell me about Interstellar 电影"]}, "create_time": 1700000200.0}}
            }
        }"#;
        ingest_chatgpt(&mut conn, updated.as_bytes(), "acct", "ws", "test").unwrap();

        let hits = search_chatgpt(&conn, "电影", 20, None, None).unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].title, "Updated title");
        // old snapshot's message should be gone
        let hits_old = search_chatgpt(&conn, "Project Hail Mary", 20, None, None).unwrap();
        assert!(hits_old.is_empty());
    }

    #[test]
    fn account_workspace_filter() {
        let db = temp_db();
        let mut conn = open_chatgpt_db(&db).unwrap();
        ingest_chatgpt(&mut conn, fixture_json().as_bytes(), "acct1", "ws1", "test").unwrap();
        // same conversation_id but different account/workspace -> distinct row
        ingest_chatgpt(&mut conn, fixture_json().as_bytes(), "acct2", "ws1", "test").unwrap();

        let all = search_chatgpt(&conn, "电影", 20, None, None).unwrap();
        assert_eq!(all.len(), 2);

        let only1 = search_chatgpt(&conn, "电影", 20, Some("acct1"), None).unwrap();
        assert_eq!(only1.len(), 1);

        let only2 = search_chatgpt(&conn, "电影", 20, Some("acct2"), Some("ws1")).unwrap();
        assert_eq!(only2.len(), 1);
    }

    #[test]
    fn doctor_reports_counts() {
        let db = temp_db();
        let mut conn = open_chatgpt_db(&db).unwrap();
        ingest_chatgpt(&mut conn, fixture_json().as_bytes(), "acct", "ws", "test").unwrap();
        let conversations: i64 = conn
            .query_row("SELECT COUNT(*) FROM conversations", [], |r| r.get(0))
            .unwrap();
        assert_eq!(conversations, 1);
        let messages: i64 = conn
            .query_row("SELECT COUNT(*) FROM messages", [], |r| r.get(0))
            .unwrap();
        assert_eq!(messages, 2);
        let docs: i64 = conn
            .query_row("SELECT COUNT(*) FROM search_documents", [], |r| r.get(0))
            .unwrap();
        assert_eq!(docs, 2);
        let _ = (conversations, messages, docs);
    }

    #[test]
    fn short_cjk_query_classification() {
        assert!(is_short_cjk_query("电影"));
        assert!(is_short_cjk_query("电"));
        assert!(!is_short_cjk_query("电影推荐系统"));
        assert!(!is_short_cjk_query("Project"));
    }

    #[test]
    fn route_matcher_is_exact_for_conversation_detail() {
        assert!(route_matches_chatgpt_conversation(
            "https://chatgpt.com/backend-api/conversation/abc-123"
        ));
        assert!(route_matches_chatgpt_conversation(
            "https://chatgpt.com/backend-api/conversation/abc-123?foo=bar"
        ));
        assert!(!route_matches_chatgpt_conversation(
            "https://chatgpt.com/backend-api/conversation/abc-123/textdocs"
        ));
        assert!(!route_matches_chatgpt_conversation(
            "https://chatgpt.com/backend-api/conversation/abc-123/stream_status"
        ));
        assert!(!route_matches_chatgpt_conversation(
            "https://chatgpt.com/backend-api/files/file_123/simple"
        ));
        assert!(!route_matches_chatgpt_conversation(
            "https://chatgpt.com/ces/v1/t"
        ));
        assert!(!route_matches_chatgpt_conversation(
            "https://example.com/backend-api/conversation/abc-123"
        ));
    }

    #[test]
    fn payload_guard_requires_mapping_and_string_id() {
        let good: Value = serde_json::from_str(fixture_json()).unwrap();
        assert!(payload_guard(&good));
        let missing_mapping: Value = serde_json::json!({"id":"conv"});
        assert!(!payload_guard(&missing_mapping));
        let non_string_id: Value = serde_json::json!({"id": 1, "mapping": {}});
        assert!(!payload_guard(&non_string_id));
        let conversation_id_ok: Value = serde_json::json!({"conversation_id":"conv","mapping":{}});
        assert!(payload_guard(&conversation_id_ok));
    }

    fn ingest_http_request(body: String, auth: Option<&str>, origin: Option<&str>) -> HttpRequest {
        let mut headers = Vec::new();
        if let Some(auth) = auth {
            headers.push(("Authorization".to_string(), auth.to_string()));
        }
        if let Some(origin) = origin {
            headers.push(("Origin".to_string(), origin.to_string()));
        }
        HttpRequest {
            method: "POST".to_string(),
            target: "/ingest/chatgpt/conversation".to_string(),
            headers,
            body: body.into_bytes(),
        }
    }

    #[test]
    fn token_and_origin_checks_reject_bad_requests() {
        assert!(token_matches(Some("secret"), "secret"));
        assert!(!token_matches(Some("wrong"), "secret"));
        assert!(!token_matches(None, "secret"));
        assert_eq!(
            bearer_token(Some("Bearer secret")).as_deref(),
            Some("secret")
        );
        assert_eq!(bearer_token(Some("secret")), None);
        assert!(chatgpt_origin_ok(None));
        assert!(chatgpt_origin_ok(Some(ALLOWED_ORIGIN)));
        assert!(!chatgpt_origin_ok(Some("https://evil.example")));
    }

    #[test]
    fn cors_preflight_allows_only_chatgpt_origin() {
        let mut allowed = Vec::new();
        cors_preflight_response(&mut allowed, true, "POST, OPTIONS").unwrap();
        let allowed = String::from_utf8(allowed).unwrap();
        assert!(allowed.starts_with("HTTP/1.1 204"));
        assert!(allowed.contains("Access-Control-Allow-Origin: https://chatgpt.com"));
        assert!(allowed.contains("Access-Control-Allow-Headers: Authorization, Content-Type"));

        let mut denied = Vec::new();
        cors_preflight_response(&mut denied, false, "POST, OPTIONS").unwrap();
        let denied = String::from_utf8(denied).unwrap();
        assert!(denied.starts_with("HTTP/1.1 403"));
        assert!(!denied.contains("Access-Control-Allow-Origin"));
    }

    #[test]
    fn ingest_handler_stores_payload_and_search_finds_movie() {
        let db = temp_db();
        let payload: Value = serde_json::from_str(fixture_json()).unwrap();
        let body = serde_json::json!({
            "account_id": "acct",
            "workspace_id": "ws",
            "source": "test",
            "url": "https://chatgpt.com/backend-api/conversation/conv-001",
            "route": "https://chatgpt.com/backend-api/conversation/conv-001",
            "payload": payload
        })
        .to_string();
        let req = ingest_http_request(body, Some("Bearer secret"), Some(ALLOWED_ORIGIN));
        let (cors, response) = handle_ingest(&req, &db, "secret").unwrap();
        assert!(cors);
        assert!(response.contains("\"ok\":true"));

        let conn = open_chatgpt_db(&db).unwrap();
        let hits = search_chatgpt(&conn, "电影", 20, Some("acct"), Some("ws")).unwrap();
        assert_eq!(hits.len(), 1);
        assert!(hits[0].snippet.contains("电影"));
    }

    #[test]
    fn ingest_handler_rejects_missing_payload_wrong_token_and_bad_route() {
        let db = temp_db();
        let missing_payload = ingest_http_request(
            serde_json::json!({"account_id":"acct"}).to_string(),
            Some("Bearer secret"),
            Some(ALLOWED_ORIGIN),
        );
        assert_eq!(
            handle_ingest(&missing_payload, &db, "secret")
                .unwrap_err()
                .0,
            400
        );

        let payload: Value = serde_json::from_str(fixture_json()).unwrap();
        let body = serde_json::json!({"payload": payload}).to_string();
        let wrong_token =
            ingest_http_request(body.clone(), Some("Bearer wrong"), Some(ALLOWED_ORIGIN));
        assert_eq!(
            handle_ingest(&wrong_token, &db, "secret").unwrap_err().0,
            401
        );

        let bad_origin = ingest_http_request(
            body.clone(),
            Some("Bearer secret"),
            Some("https://evil.example"),
        );
        assert_eq!(
            handle_ingest(&bad_origin, &db, "secret").unwrap_err().0,
            403
        );

        let payload: Value = serde_json::from_str(fixture_json()).unwrap();
        let bad_route_body = serde_json::json!({
            "url": "https://chatgpt.com/backend-api/conversation/conv-001/textdocs",
            "payload": payload
        })
        .to_string();
        let bad_route =
            ingest_http_request(bad_route_body, Some("Bearer secret"), Some(ALLOWED_ORIGIN));
        assert_eq!(handle_ingest(&bad_route, &db, "secret").unwrap_err().0, 400);
    }

    #[test]
    fn userscript_default_does_not_embed_token_and_main_never_contains_it() {
        let token = "secret-token-for-test";
        let main = main_world_script();
        let sender_without_token = sender_script("http://127.0.0.1:37531", None);
        let sender_with_token = sender_script("http://127.0.0.1:37531", Some(token));

        assert!(!main.contains(token));
        assert!(!sender_without_token.contains(token));
        assert!(sender_with_token.contains(token));
        assert!(!main.contains("Authorization"));
        assert!(sender_with_token.contains("Authorization"));
    }

    #[test]
    fn validate_loopback_server_accepts_loopback_and_rejects_others() {
        // accepted loopback forms
        assert!(validate_loopback_server("http://127.0.0.1:37531").is_ok());
        assert!(validate_loopback_server("http://localhost:37531").is_ok());
        assert!(validate_loopback_server("http://localhost:37531/").is_ok());
        assert!(validate_loopback_server("http://[::1]:37531").is_ok());
        assert!(validate_loopback_server("http://127.0.0.1:37531?foo=bar").is_ok());

        // rejected forms
        assert!(validate_loopback_server("https://127.0.0.1:37531").is_err());
        assert!(validate_loopback_server("http://1.2.3.4:37531").is_err());
        assert!(validate_loopback_server("http://127.0.0.1:37531/path").is_err());
        assert!(validate_loopback_server("http://127.0.0.1:37531#frag").is_err());
        assert!(validate_loopback_server("http://user:pass@127.0.0.1:37531").is_err());
        assert!(validate_loopback_server("http://127.0.0.1").is_err());
        assert!(validate_loopback_server("http://example.com:37531").is_err());
    }

    #[test]
    fn print_embed_token_rejects_non_loopback_before_reading_token_file() {
        // Point at a token file that does not exist. The non-loopback server
        // must be rejected BEFORE the missing token file is ever read, so the
        // error must mention the server, not the token file.
        let missing = env::temp_dir().join("chat-memory-no-such-token-file-print");
        let _ = fs::remove_file(&missing);
        let err = chatgpt_userscript(
            &dummy_config(),
            UserscriptAction::Print,
            "http://1.2.3.4:37531",
            &missing,
            true,
        )
        .unwrap_err();
        assert!(err.contains("loopback") || err.contains("server"), "{err}");
        assert!(!err.contains("token file"), "{err}");
    }

    #[test]
    fn install_rejects_non_loopback_before_reading_token_file() {
        let missing = env::temp_dir().join("chat-memory-no-such-token-file-install");
        let _ = fs::remove_file(&missing);
        let err = chatgpt_userscript(
            &dummy_config(),
            UserscriptAction::Install,
            "http://1.2.3.4:37531",
            &missing,
            false,
        )
        .unwrap_err();
        assert!(err.contains("loopback") || err.contains("server"), "{err}");
        assert!(!err.contains("token file"), "{err}");
        assert!(!missing.exists(), "install must not create the token file");
    }

    #[test]
    fn install_token_read_is_deterministic_for_missing_and_empty() {
        let missing = env::temp_dir().join("chat-memory-missing-token-read");
        let _ = fs::remove_file(&missing);
        let err = read_token_file(&missing).unwrap_err();
        assert!(err.contains("token file") || err.contains("cannot read"));

        let empty = env::temp_dir().join("chat-memory-empty-token-read");
        fs::write(&empty, b"   \n").unwrap();
        let err = read_token_file(&empty).unwrap_err();
        assert!(err.contains("empty"), "{err}");
        let _ = fs::remove_file(&empty);
    }

    #[test]
    fn bro_registration_payload_has_two_scripts_correct_fields_and_token_only_in_sender() {
        let token = "test-ingest-token-abc";
        let main_code = main_world_code();
        let sender_code = sender_code("http://127.0.0.1:37531", token);
        let scripts = build_bro_registration_scripts(&main_code, &sender_code);

        assert_eq!(scripts.len(), 2);
        let main = &scripts[0];
        let sender = &scripts[1];
        assert_eq!(main["id"], CHATGPT_MAIN_SCRIPT_ID);
        assert_eq!(sender["id"], CHATGPT_SENDER_SCRIPT_ID);
        assert_eq!(main["world"], "MAIN");
        assert_eq!(sender["world"], "USER_SCRIPT");
        assert_eq!(main["runAt"], "document_start");
        assert_eq!(sender["runAt"], "document_start");
        assert_eq!(main["allFrames"], false);
        assert_eq!(sender["allFrames"], false);
        assert_eq!(main["matches"][0], "https://chatgpt.com/*");
        assert_eq!(sender["matches"][0], "https://chatgpt.com/*");

        let main_js = main["js"][0]["code"].as_str().unwrap();
        let sender_js = sender["js"][0]["code"].as_str().unwrap();
        assert!(
            !main_js.contains(token),
            "MAIN world must never contain the token"
        );
        assert!(
            sender_js.contains(token),
            "USER_SCRIPT sender must contain the token"
        );
        assert!(!main_js.contains("Authorization"));
        assert!(sender_js.contains("Authorization"));
    }

    #[test]
    fn sender_repeats_route_and_payload_guards() {
        let token = "tok";
        let code = sender_code("http://127.0.0.1:37531", token);
        // route guard repeated in sender
        assert!(code.contains("isConversationUrl"));
        assert!(code.contains("isListUrl"));
        assert!(code.contains("ROUTE_RE"));
        assert!(code.contains("backend-api") && code.contains("conversation"));
        // payload guard repeated in sender
        assert!(code.contains("guardPayload"));
        assert!(code.contains("mapping"));
        // re-validation happens before the POST
        let guard_pos = code.find("if (!guardPayload(payload))").unwrap();
        let post_pos = code.find("/ingest/chatgpt/conversation").unwrap();
        assert!(guard_pos < post_pos, "payload guard must run before ingest");
        // only the local token is forwarded; no page cookie/header forwarding
        assert!(code.contains("opts.headers[\"Authorization\"] = \"Bearer \" + TOKEN"));
        assert!(!code.contains("document.cookie"));
    }

    #[test]
    fn userscript_adapter_calls_refresh_endpoints_and_discards_error_bodies() {
        let main = main_world_code();
        let sender = sender_code("http://127.0.0.1:37531", "tok");

        assert!(main.contains("chat-memory-list"));
        assert!(main.contains("chat-memory-event"));
        assert!(main.contains("emitNavigation"));
        assert!(main.contains("maybeDirty"));
        assert!(main.contains("reduceList"));

        assert!(sender.contains("/ingest/chatgpt/list"));
        assert!(sender.contains("/events/chatgpt"));
        assert!(sender.contains("/refresh/chatgpt/lease"));
        assert!(sender.contains("/refresh/chatgpt/report"));
        assert!(sender.contains("MAX_DETAIL_LEASES_PER_TAB = 20"));
        assert!(sender.contains("reduceListItems"));
        assert!(sender.contains("reportLease(lease.lease_id, false, resp.status"));
        assert!(sender.contains("if (!(resp.status >= 200 && resp.status < 300))"));
        assert!(
            !sender.contains("responseText"),
            "sender must not forward XHR response bodies"
        );
        assert!(!sender.contains("document.cookie"));
    }

    #[test]
    fn js_string_escape_prevents_injection() {
        let out = js_string_escape("a\"b\\c\n d");
        assert_eq!(out, "a\\\"b\\\\c\\n d");
        // a token with a quote cannot break out of the JS string
        let code = sender_code("http://127.0.0.1:37531", "ev\"il");
        assert!(code.contains("\"ev\\\"il\""));
    }

    // A fake MCP client that records every call and never touches the network
    // or `Command`. Used to prove the install orchestration and payload
    // construction are testable without a real bro and that the token-bearing
    // payload only travels through the `userscripts_register` tool arguments.
    struct FakeMcpClient {
        calls: Vec<(String, Value)>,
        list_result: Value,
        register_ok: bool,
    }
    impl McpClient for FakeMcpClient {
        fn initialize(&mut self) -> Result<Value, String> {
            self.calls
                .push(("initialize".to_string(), mcp_initialize_params()));
            Ok(serde_json::json!({ "protocolVersion": "2024-11-05" }))
        }
        fn call_tool(&mut self, name: &str, args: &Value) -> Result<Value, String> {
            self.calls.push((name.to_string(), args.clone()));
            match name {
                "userscripts_register" if !self.register_ok => Err("register failed".to_string()),
                "userscripts_list" => Ok(self.list_result.clone()),
                _ => Ok(serde_json::json!({})),
            }
        }
    }

    fn both_ids_list_result() -> Value {
        serde_json::json!({
            "content": [
                { "type": "text", "text": "[\"chat-memory-chatgpt-main\",\"chat-memory-chatgpt-sender\"]" }
            ],
            "isError": false
        })
    }

    #[test]
    fn install_orchestration_registers_and_verifies_via_mcp_trait() {
        let token = "secret-install-token";
        let scripts = build_bro_registration_scripts(
            &main_world_code(),
            &sender_code("http://127.0.0.1:37531", token),
        );
        let ids = [CHATGPT_MAIN_SCRIPT_ID, CHATGPT_SENDER_SCRIPT_ID];
        let mut client = FakeMcpClient {
            calls: Vec::new(),
            list_result: both_ids_list_result(),
            register_ok: true,
        };
        install_userscripts_via_bro(&mut client, &scripts, &ids).unwrap();

        let names: Vec<&str> = client.calls.iter().map(|(n, _)| n.as_str()).collect();
        assert_eq!(
            names,
            vec![
                "initialize",
                "userscripts_unregister",
                "userscripts_register",
                "userscripts_list"
            ]
        );

        // Token only appears in the register call, never in init/unregister/list.
        for (name, args) in &client.calls {
            let blob = args.to_string();
            if name == "userscripts_register" {
                assert!(
                    blob.contains(token),
                    "register must carry the token-bearing sender"
                );
            } else {
                assert!(!blob.contains(token), "token leaked into {name}: {blob}");
            }
        }

        // The register arguments contain exactly two scripts with token only
        // in the USER_SCRIPT sender code.
        let reg = client
            .calls
            .iter()
            .find(|(n, _)| n == "userscripts_register")
            .unwrap();
        let reg_scripts = reg.1.get("scripts").unwrap().as_array().unwrap();
        assert_eq!(reg_scripts.len(), 2);
        let main_js = reg_scripts[0]["js"][0]["code"].as_str().unwrap();
        let sender_js = reg_scripts[1]["js"][0]["code"].as_str().unwrap();
        assert!(!main_js.contains(token));
        assert!(sender_js.contains(token));
    }

    #[test]
    fn install_fails_when_register_errors() {
        let scripts = build_bro_registration_scripts(
            &main_world_code(),
            &sender_code("http://127.0.0.1:37531", "t"),
        );
        let ids = [CHATGPT_MAIN_SCRIPT_ID, CHATGPT_SENDER_SCRIPT_ID];
        let mut client = FakeMcpClient {
            calls: Vec::new(),
            list_result: both_ids_list_result(),
            register_ok: false,
        };
        let err = install_userscripts_via_bro(&mut client, &scripts, &ids).unwrap_err();
        assert!(err.contains("userscripts_register"), "{err}");
    }

    #[test]
    fn install_fails_when_verification_missing_id() {
        let scripts = build_bro_registration_scripts(
            &main_world_code(),
            &sender_code("http://127.0.0.1:37531", "t"),
        );
        let ids = [CHATGPT_MAIN_SCRIPT_ID, CHATGPT_SENDER_SCRIPT_ID];
        // list result only mentions one id
        let list_result = serde_json::json!({
            "content": [ { "type": "text", "text": "[\"chat-memory-chatgpt-main\"]" } ]
        });
        let mut client = FakeMcpClient {
            calls: Vec::new(),
            list_result,
            register_ok: true,
        };
        let err = install_userscripts_via_bro(&mut client, &scripts, &ids).unwrap_err();
        assert!(err.contains("verification failed"), "{err}");
        assert!(err.contains(CHATGPT_SENDER_SCRIPT_ID), "{err}");
    }

    #[test]
    fn mcp_request_builders_have_expected_shape() {
        let unreg = mcp_userscripts_unregister_args(&["a", "b"]);
        assert_eq!(unreg["ids"][0], "a");
        assert_eq!(unreg["ids"][1], "b");

        let list = mcp_userscripts_list_args(&["a"]);
        assert_eq!(list["ids"][0], "a");

        let scripts = build_bro_registration_scripts(
            &main_world_code(),
            &sender_code("http://127.0.0.1:37531", "t"),
        );
        let reg = mcp_userscripts_register_args(&scripts);
        assert_eq!(reg["scripts"].as_array().unwrap().len(), 2);

        let init = mcp_initialize_params();
        assert_eq!(init["clientInfo"]["name"], "chat-memory");
    }

    #[test]
    fn parse_http_url_parses_loopback_endpoint() {
        let (host, port, path) = parse_http_url("http://127.0.0.1:3500/mcp").unwrap();
        assert_eq!(host, "127.0.0.1");
        assert_eq!(port, 3500);
        assert_eq!(path, "/mcp");
        let (host, port, path) = parse_http_url("http://localhost:3500").unwrap();
        assert_eq!(host, "localhost");
        assert_eq!(port, 3500);
        assert_eq!(path, "/");
    }

    #[test]
    fn parse_http_response_decodes_json_and_chunked() {
        let raw = b"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: 11\r\n\r\n{\"ok\":true}!!";
        let (headers, body) = parse_http_response(raw).unwrap();
        assert_eq!(http_status(&headers).unwrap(), 200);
        assert_eq!(body, b"{\"ok\":true}");

        let chunked = b"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nTransfer-Encoding: chunked\r\n\r\n5\r\n{\"ok\"\r\n6\r\n:true}\r\n0\r\n\r\n";
        let (_headers, body) = parse_http_response(chunked).unwrap();
        assert_eq!(body, b"{\"ok\":true}");
    }

    #[test]
    fn extract_sse_json_parses_data_event() {
        let sse = "event: message\r\ndata: {\"jsonrpc\":\"2.0\",\"id\":1,\"result\":{\"ok\":true}}\r\n\r\n";
        let v = extract_sse_json(sse).unwrap();
        assert_eq!(v["result"]["ok"], true);
    }

    #[test]
    fn extract_sse_json_skips_empty_bro_preamble_event() {
        let sse = "data: \nid: 0\nretry: 3000\n\ndata: {\"jsonrpc\":\"2.0\",\"id\":1,\"result\":{\"ok\":true}}\n\n";
        let v = extract_sse_json(sse).unwrap();
        assert_eq!(v["result"]["ok"], true);
    }

    #[test]
    fn parse_mcp_response_handles_json_and_reports_errors() {
        let headers = vec![
            (":status".to_string(), "200".to_string()),
            ("content-type".to_string(), "application/json".to_string()),
        ];
        let body = br#"{"jsonrpc":"2.0","id":1,"result":{"x":1}}"#;
        let v = parse_mcp_response(&headers, body).unwrap();
        assert_eq!(v["x"], 1);

        let err_body = br#"{"jsonrpc":"2.0","id":1,"error":{"message":"boom"}}"#;
        let err = parse_mcp_response(&headers, err_body).unwrap_err();
        assert!(err.contains("boom"));
    }
    // ---------- Slice 1a / 2a helpers and tests ----------

    fn open_test_conn(db: &Path) -> Connection {
        open_chatgpt_db(db).unwrap()
    }

    fn list_request(body: &str, auth: Option<&str>, origin: Option<&str>) -> HttpRequest {
        let mut headers = Vec::new();
        if let Some(a) = auth {
            headers.push(("Authorization".to_string(), a.to_string()));
        }
        if let Some(o) = origin {
            headers.push(("Origin".to_string(), o.to_string()));
        }
        HttpRequest {
            method: "POST".to_string(),
            target: "/ingest/chatgpt/list".to_string(),
            headers,
            body: body.as_bytes().to_vec(),
        }
    }

    fn event_request(body: &str, auth: Option<&str>, origin: Option<&str>) -> HttpRequest {
        let mut headers = Vec::new();
        if let Some(a) = auth {
            headers.push(("Authorization".to_string(), a.to_string()));
        }
        if let Some(o) = origin {
            headers.push(("Origin".to_string(), o.to_string()));
        }
        HttpRequest {
            method: "POST".to_string(),
            target: "/events/chatgpt".to_string(),
            headers,
            body: body.as_bytes().to_vec(),
        }
    }

    fn lease_request(auth: Option<&str>, origin: Option<&str>) -> HttpRequest {
        let mut headers = Vec::new();
        if let Some(a) = auth {
            headers.push(("Authorization".to_string(), a.to_string()));
        }
        if let Some(o) = origin {
            headers.push(("Origin".to_string(), o.to_string()));
        }
        HttpRequest {
            method: "GET".to_string(),
            target: "/refresh/chatgpt/lease".to_string(),
            headers,
            body: Vec::new(),
        }
    }

    fn report_request(body: &str, auth: Option<&str>, origin: Option<&str>) -> HttpRequest {
        let mut headers = Vec::new();
        if let Some(a) = auth {
            headers.push(("Authorization".to_string(), a.to_string()));
        }
        if let Some(o) = origin {
            headers.push(("Origin".to_string(), o.to_string()));
        }
        HttpRequest {
            method: "POST".to_string(),
            target: "/refresh/chatgpt/report".to_string(),
            headers,
            body: body.as_bytes().to_vec(),
        }
    }

    fn err_status(res: Result<(bool, String), (u16, String)>) -> u16 {
        res.unwrap_err().0
    }

    #[test]
    fn additive_migration_from_minimal_pre_v2_db() {
        let db = temp_db();
        // Create a minimal pre-v2 conversations table with only the core columns.
        {
            let conn = Connection::open(&db).unwrap();
            conn.execute_batch(
                "CREATE TABLE conversations (
                    conversation_pk INTEGER PRIMARY KEY AUTOINCREMENT,
                    account_id TEXT NOT NULL,
                    workspace_id TEXT NOT NULL,
                    remote_conversation_id TEXT NOT NULL,
                    UNIQUE(account_id, workspace_id, remote_conversation_id)
                );
                CREATE TABLE refresh_queue (
                    conversation_pk INTEGER NOT NULL,
                    reason TEXT NOT NULL,
                    PRIMARY KEY(conversation_pk, reason)
                );",
            )
            .unwrap();
            conn.execute(
                "INSERT INTO conversations (account_id, workspace_id, remote_conversation_id)
                 VALUES ('a','w','old-1')",
                [],
            )
            .unwrap();
        }
        // Now open via the real schema path; migration must add missing columns.
        let conn = open_test_conn(&db);
        // The new columns should exist and have defaults.
        let (vis, fresh, failures): (String, String, i64) = conn
            .query_row(
                "SELECT visibility_state, freshness_state, consecutive_failures
                 FROM conversations WHERE remote_conversation_id = 'old-1'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .unwrap();
        assert_eq!(vis, "unknown");
        assert_eq!(fresh, "unknown");
        assert_eq!(failures, 0);
        // New tables exist.
        let n: i64 = conn
            .query_row("SELECT COUNT(*) FROM service_state", [], |r| r.get(0))
            .unwrap();
        assert_eq!(n, 0);
        let n: i64 = conn
            .query_row("SELECT COUNT(*) FROM refresh_leases", [], |r| r.get(0))
            .unwrap();
        assert_eq!(n, 0);
        // refresh_queue gained columns.
        let cols: Vec<String> = conn
            .prepare("PRAGMA table_info(refresh_queue)")
            .unwrap()
            .query_map([], |r| r.get::<_, String>(1))
            .unwrap()
            .filter_map(Result::ok)
            .collect();
        assert!(cols.iter().any(|c| c == "priority"));
        assert!(cols.iter().any(|c| c == "not_before"));
        assert!(cols.iter().any(|c| c == "attempt_count"));
    }

    #[test]
    fn list_ingest_upserts_metadata_and_does_not_create_snapshots_or_docs() {
        let db = temp_db();
        let body = serde_json::json!({
            "source": "userscript:list-capture",
            "account_id": "acct",
            "workspace_id": "ws",
            "items": [
                {"id": "conv-a", "title": "Alpha", "create_time": 100.0, "update_time": 200.0},
                {"id": "conv-b", "title": "Beta", "update_time": 300.0},
            ]
        })
        .to_string();
        let req = list_request(&body, Some("Bearer secret"), Some(ALLOWED_ORIGIN));
        let (cors, resp) = handle_list_ingest(&req, &db, "secret").unwrap();
        assert!(cors);
        assert!(resp.contains("\"ok\":true"));
        assert!(resp.contains("\"seen\":2"));
        assert!(resp.contains("\"upserted\":2"));
        assert!(resp.contains("\"queued\":2"));

        let conn = open_test_conn(&db);
        let convs: i64 = conn
            .query_row("SELECT COUNT(*) FROM conversations", [], |r| r.get(0))
            .unwrap();
        assert_eq!(convs, 2);
        let snaps: i64 = conn
            .query_row("SELECT COUNT(*) FROM conversation_snapshots", [], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(snaps, 0);
        let docs: i64 = conn
            .query_row("SELECT COUNT(*) FROM search_documents", [], |r| r.get(0))
            .unwrap();
        assert_eq!(docs, 0);
        let queue: i64 = conn
            .query_row("SELECT COUNT(*) FROM refresh_queue", [], |r| r.get(0))
            .unwrap();
        assert_eq!(queue, 2);
        // last_seen_in_list_at is set.
        let seen: f64 = conn
            .query_row(
                "SELECT last_seen_in_list_at FROM conversations WHERE remote_conversation_id='conv-a'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert!(seen > 0.0);
    }

    #[test]
    fn list_ingest_ignores_body_like_fields_and_does_not_echo_them() {
        let db = temp_db();
        let body = serde_json::json!({
            "items": [
                {
                    "id": "conv-x",
                    "title": "X",
                    "mapping": {"root": {"big": "object"}},
                    "messages": [{"role": "user", "content": {"parts": ["secret text"]}}],
                    "content": {"parts": ["more text"]},
                    "parts": ["p1", "p2"],
                    "text": "raw body text",
                    "attachments": [{"foo": 1}],
                    "nested": {"deep": {"object": [1, 2, 3]}}
                }
            ]
        })
        .to_string();
        let req = list_request(&body, Some("Bearer secret"), Some(ALLOWED_ORIGIN));
        let (_cors, resp) = handle_list_ingest(&req, &db, "secret").unwrap();
        // Response is counts only; never echoes raw item JSON or body fields.
        assert!(!resp.contains("mapping"));
        assert!(!resp.contains("secret text"));
        assert!(!resp.contains("raw body text"));
        assert!(!resp.contains("attachments"));
        assert!(resp.contains("\"ok\":true"));

        let conn = open_test_conn(&db);
        // No snapshots/docs created despite body-like fields present.
        let snaps: i64 = conn
            .query_row("SELECT COUNT(*) FROM conversation_snapshots", [], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(snaps, 0);
        let docs: i64 = conn
            .query_row("SELECT COUNT(*) FROM search_documents", [], |r| r.get(0))
            .unwrap();
        assert_eq!(docs, 0);
        // Title was stored from the scalar field.
        let title: String = conn
            .query_row(
                "SELECT title FROM conversations WHERE remote_conversation_id='conv-x'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(title, "X");
    }

    #[test]
    fn list_ingest_enforces_token_origin_and_body_limit() {
        let db = temp_db();
        let body = serde_json::json!({"items":[{"id":"c"}]}).to_string();

        // Missing token -> 401
        let req = list_request(&body, None, Some(ALLOWED_ORIGIN));
        assert_eq!(err_status(handle_list_ingest(&req, &db, "secret")), 401);
        // Wrong token -> 401
        let req = list_request(&body, Some("Bearer wrong"), Some(ALLOWED_ORIGIN));
        assert_eq!(err_status(handle_list_ingest(&req, &db, "secret")), 401);
        // Bad origin -> 403
        let req = list_request(&body, Some("Bearer secret"), Some("https://evil.example"));
        assert_eq!(err_status(handle_list_ingest(&req, &db, "secret")), 403);
        // No origin (local test) -> ok
        let req = list_request(&body, Some("Bearer secret"), None);
        let (cors, _) = handle_list_ingest(&req, &db, "secret").unwrap();
        assert!(!cors);

        // Body too large -> 413
        let big = serde_json::json!({
            "items": [{"id": "x".repeat(300_000)}]
        })
        .to_string();
        let req = list_request(&big, Some("Bearer secret"), Some(ALLOWED_ORIGIN));
        assert_eq!(err_status(handle_list_ingest(&req, &db, "secret")), 413);
    }

    #[test]
    fn new_endpoints_preflight_origin_and_methods() {
        let mut buf = Vec::new();
        cors_preflight_response(&mut buf, true, "POST, OPTIONS").unwrap();
        let s = String::from_utf8(buf).unwrap();
        assert!(s.contains("Access-Control-Allow-Methods: POST, OPTIONS"));

        let mut buf = Vec::new();
        cors_preflight_response(&mut buf, true, "GET, OPTIONS").unwrap();
        let s = String::from_utf8(buf).unwrap();
        assert!(s.contains("Access-Control-Allow-Methods: GET, OPTIONS"));
        assert!(s.contains("Access-Control-Allow-Origin: https://chatgpt.com"));
        assert!(s.contains("Vary: Origin"));

        // Denied preflight has no CORS headers.
        let mut buf = Vec::new();
        cors_preflight_response(&mut buf, false, "POST, OPTIONS").unwrap();
        let s = String::from_utf8(buf).unwrap();
        assert!(s.starts_with("HTTP/1.1 403"));
        assert!(!s.contains("Access-Control-Allow-Origin"));
    }

    #[test]
    fn events_and_lease_endpoints_require_token_and_origin() {
        let db = temp_db();
        let ev = serde_json::json!({"kind":"adapter_hello"}).to_string();

        // events: no token -> 401
        assert_eq!(
            err_status(handle_events(
                &event_request(&ev, None, Some(ALLOWED_ORIGIN)),
                &db,
                "secret"
            )),
            401
        );
        // events: bad origin -> 403
        assert_eq!(
            err_status(handle_events(
                &event_request(&ev, Some("Bearer secret"), Some("https://evil.example")),
                &db,
                "secret"
            )),
            403
        );
        // lease: no token -> 401
        assert_eq!(
            err_status(handle_lease(
                &lease_request(None, Some(ALLOWED_ORIGIN)),
                &db,
                "secret"
            )),
            401
        );
        // lease: bad origin -> 403
        assert_eq!(
            err_status(handle_lease(
                &lease_request(Some("Bearer secret"), Some("https://evil.example")),
                &db,
                "secret"
            )),
            403
        );
        // report: no token -> 401
        let rep = serde_json::json!({"lease_id":"x","ok":true}).to_string();
        assert_eq!(
            err_status(handle_report(
                &report_request(&rep, None, Some(ALLOWED_ORIGIN)),
                &db,
                "secret"
            )),
            401
        );
    }

    #[test]
    fn navigation_event_creates_known_row_and_one_opened_queue_row() {
        let db = temp_db();
        let body = serde_json::json!({
            "kind": "navigation",
            "conversation_id": "nav-1",
            "url": "https://chatgpt.com/c/nav-1",
            "reason": "opened"
        })
        .to_string();
        let req = event_request(&body, Some("Bearer secret"), Some(ALLOWED_ORIGIN));
        let (cors, resp) = handle_events(&req, &db, "secret").unwrap();
        assert!(cors);
        assert!(resp.contains("\"ok\":true"));

        let conn = open_test_conn(&db);
        let convs: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM conversations WHERE remote_conversation_id='nav-1'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(convs, 1);
        let queue: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM refresh_queue WHERE reason='opened'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(queue, 1);
        let priority: i64 = conn
            .query_row(
                "SELECT priority FROM refresh_queue WHERE reason='opened'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(priority, OPENED_PRIORITY);
    }

    #[test]
    fn navigation_extracts_id_from_url_when_conversation_id_absent() {
        let db = temp_db();
        let body = serde_json::json!({
            "kind": "navigation",
            "url": "https://chatgpt.com/c/from-url-42"
        })
        .to_string();
        let req = event_request(&body, Some("Bearer secret"), None);
        handle_events(&req, &db, "secret").unwrap();
        let conn = open_test_conn(&db);
        let n: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM conversations WHERE remote_conversation_id='from-url-42'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(n, 1);
    }

    #[test]
    fn dirty_event_coalesces_and_updates_not_before() {
        let db = temp_db();
        let conn = open_test_conn(&db);
        // Seed a known row first via navigation.
        let body = serde_json::json!({
            "kind": "navigation",
            "conversation_id": "dirty-1"
        })
        .to_string();
        handle_events(
            &event_request(&body, Some("Bearer secret"), None),
            &db,
            "secret",
        )
        .unwrap();

        // First dirty event.
        let body = serde_json::json!({"kind":"dirty","conversation_id":"dirty-1"}).to_string();
        handle_events(
            &event_request(&body, Some("Bearer secret"), None),
            &db,
            "secret",
        )
        .unwrap();
        let nb1: f64 = conn
            .query_row(
                "SELECT not_before FROM refresh_queue WHERE reason='dirty'",
                [],
                |r| r.get(0),
            )
            .unwrap();

        // Second dirty event should coalesce (still one row) and push not_before later.
        std::thread::sleep(std::time::Duration::from_millis(1100));
        handle_events(
            &event_request(&body, Some("Bearer secret"), None),
            &db,
            "secret",
        )
        .unwrap();
        let count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM refresh_queue WHERE reason='dirty'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(count, 1);
        let nb2: f64 = conn
            .query_row(
                "SELECT not_before FROM refresh_queue WHERE reason='dirty'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert!(
            nb2 >= nb1,
            "not_before should not move earlier on dirty coalesce"
        );
    }

    #[test]
    fn lease_grant_creates_one_active_lease_then_no_work() {
        let db = temp_db();
        // Seed a due refresh row via navigation.
        let body = serde_json::json!({
            "kind": "navigation",
            "conversation_id": "lease-1"
        })
        .to_string();
        handle_events(
            &event_request(&body, Some("Bearer secret"), None),
            &db,
            "secret",
        )
        .unwrap();

        let req = lease_request(Some("Bearer secret"), Some(ALLOWED_ORIGIN));
        let (cors, resp) = handle_lease(&req, &db, "secret").unwrap();
        assert!(cors);
        assert!(resp.contains("\"lease\":{"));
        assert!(resp.contains("\"type\":\"detail\""));
        assert!(resp.contains("/backend-api/conversation/lease-1"));
        assert!(resp.contains("\"deadline_ms\":30000"));

        // A second call while one active lease exists returns no work.
        let req = lease_request(Some("Bearer secret"), Some(ALLOWED_ORIGIN));
        let (_cors2, resp2) = handle_lease(&req, &db, "secret").unwrap();
        assert!(resp2.contains("\"lease\":null"));
        assert!(resp2.contains("\"poll_after_ms\":5000"));
    }

    #[test]
    fn lease_returns_no_work_when_queue_empty() {
        let db = temp_db();
        let req = lease_request(Some("Bearer secret"), None);
        let (_, resp) = handle_lease(&req, &db, "secret").unwrap();
        assert!(resp.contains("\"lease\":null"));
    }

    #[test]
    fn report_429_sets_cooldown_and_prevents_further_lease() {
        let db = temp_db();
        // Seed + grant a lease.
        let body = serde_json::json!({"kind":"navigation","conversation_id":"r429"}).to_string();
        handle_events(
            &event_request(&body, Some("Bearer secret"), None),
            &db,
            "secret",
        )
        .unwrap();
        let (_, lease_resp) =
            handle_lease(&lease_request(Some("Bearer secret"), None), &db, "secret").unwrap();
        // Extract lease_id from response.
        let v: Value = serde_json::from_str(&lease_resp).unwrap();
        let lease_id = v["lease"]["lease_id"].as_str().unwrap().to_string();

        // Report 429.
        let rep = serde_json::json!({
            "lease_id": lease_id,
            "ok": false,
            "status": 429,
            "retry_after_ms": 60000,
            "error": "rate_limited"
        })
        .to_string();
        let req = report_request(&rep, Some("Bearer secret"), None);
        let (_, rresp) = handle_report(&req, &db, "secret").unwrap();
        assert!(rresp.contains("\"ok\":true"));

        // Further lease grant is blocked by global cooldown.
        let (_, resp2) =
            handle_lease(&lease_request(Some("Bearer secret"), None), &db, "secret").unwrap();
        assert!(resp2.contains("\"lease\":null"));
        assert!(resp2.contains("\"poll_after_ms\":"));
        let conn = open_test_conn(&db);
        let cd: f64 = get_service_state_f64(&conn, "global_cooldown_until").unwrap();
        assert!(cd > now_secs());
    }

    #[test]
    fn report_rejects_forbidden_body_headers_and_token_fields() {
        let db = temp_db();
        // Seed + grant.
        let body = serde_json::json!({"kind":"navigation","conversation_id":"rforb"}).to_string();
        handle_events(
            &event_request(&body, Some("Bearer secret"), None),
            &db,
            "secret",
        )
        .unwrap();
        let (_, lease_resp) =
            handle_lease(&lease_request(Some("Bearer secret"), None), &db, "secret").unwrap();
        let v: Value = serde_json::from_str(&lease_resp).unwrap();
        let lease_id = v["lease"]["lease_id"].as_str().unwrap().to_string();

        for field in [
            "body",
            "response",
            "html",
            "json",
            "headers",
            "authorization",
            "cookie",
            "accessToken",
        ] {
            let rep = serde_json::json!({
                "lease_id": lease_id,
                "ok": false,
                field: "some-chatgpt-response-content"
            })
            .to_string();
            let req = report_request(&rep, Some("Bearer secret"), None);
            let code = err_status(handle_report(&req, &db, "secret"));
            assert_eq!(code, 400, "field {field} should be rejected with 400");
        }
    }

    #[test]
    fn report_404_increments_failure_and_dirty_event_can_requeue() {
        let db = temp_db();
        // Seed + grant.
        let body = serde_json::json!({"kind":"navigation","conversation_id":"r404"}).to_string();
        handle_events(
            &event_request(&body, Some("Bearer secret"), None),
            &db,
            "secret",
        )
        .unwrap();
        let (_, lease_resp) =
            handle_lease(&lease_request(Some("Bearer secret"), None), &db, "secret").unwrap();
        let v: Value = serde_json::from_str(&lease_resp).unwrap();
        let lease_id = v["lease"]["lease_id"].as_str().unwrap().to_string();

        // Report 404 failure.
        let rep = serde_json::json!({
            "lease_id": lease_id,
            "ok": false,
            "status": 404,
            "error": "not_found"
        })
        .to_string();
        let req = report_request(&rep, Some("Bearer secret"), None);
        handle_report(&req, &db, "secret").unwrap();

        let conn = open_test_conn(&db);
        let failures: i64 = conn
            .query_row(
                "SELECT consecutive_failures FROM conversations WHERE remote_conversation_id='r404'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(failures, 1);
        // The queue row was pushed into the backoff window (not_before in future).
        let nb: f64 = conn
            .query_row(
                "SELECT not_before FROM refresh_queue WHERE conversation_pk=(
                    SELECT conversation_pk FROM conversations WHERE remote_conversation_id='r404'
                ) AND reason='opened'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert!(nb > now_secs());

        // Move the queue row's not_before into the past to simulate backoff
        // elapsing, then a dirty event should be able to enqueue (different
        // reason coalesces separately).
        conn.execute("UPDATE refresh_queue SET not_before = 0", [])
            .unwrap();
        // A dirty event creates a 'dirty' queue row (distinct reason).
        let dirty_body = serde_json::json!({"kind":"dirty","conversation_id":"r404"}).to_string();
        handle_events(
            &event_request(&dirty_body, Some("Bearer secret"), None),
            &db,
            "secret",
        )
        .unwrap();
        let dirty_count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM refresh_queue WHERE reason='dirty'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(dirty_count, 1);
        // Lease can be granted again because failures < 3.
        let (_, resp) =
            handle_lease(&lease_request(Some("Bearer secret"), None), &db, "secret").unwrap();
        assert!(
            resp.contains("\"lease\":{"),
            "lease should be granted when failures < 3: {resp}"
        );
    }

    #[test]
    fn report_success_clears_queue_and_resets_failures() {
        let db = temp_db();
        // Seed + grant.
        let body = serde_json::json!({"kind":"navigation","conversation_id":"rok"}).to_string();
        handle_events(
            &event_request(&body, Some("Bearer secret"), None),
            &db,
            "secret",
        )
        .unwrap();
        let (_, lease_resp) =
            handle_lease(&lease_request(Some("Bearer secret"), None), &db, "secret").unwrap();
        let v: Value = serde_json::from_str(&lease_resp).unwrap();
        let lease_id = v["lease"]["lease_id"].as_str().unwrap().to_string();

        let rep = serde_json::json!({"lease_id": lease_id, "ok": true}).to_string();
        let req = report_request(&rep, Some("Bearer secret"), None);
        handle_report(&req, &db, "secret").unwrap();

        let conn = open_test_conn(&db);
        let queue: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM refresh_queue WHERE conversation_pk=(
                    SELECT conversation_pk FROM conversations WHERE remote_conversation_id='rok'
                )",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(queue, 0);
        let lease_status: String = conn
            .query_row(
                "SELECT status FROM refresh_leases WHERE lease_id=?1",
                params![lease_id],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(lease_status, "succeeded");
    }

    #[test]
    fn adapter_hello_updates_last_seen() {
        let db = temp_db();
        let body = serde_json::json!({"kind":"adapter_hello"}).to_string();
        handle_events(
            &event_request(&body, Some("Bearer secret"), None),
            &db,
            "secret",
        )
        .unwrap();
        let conn = open_test_conn(&db);
        let ts: f64 = get_service_state_f64(&conn, "last_adapter_seen_at").unwrap();
        assert!(ts > 0.0);
    }

    #[test]
    fn parse_navigation_conversation_id_extracts_id_from_c_path() {
        assert_eq!(
            parse_navigation_conversation_id("https://chatgpt.com/c/abc-123"),
            Some("abc-123".to_string())
        );
        assert_eq!(
            parse_navigation_conversation_id("https://chatgpt.com/c/abc-123?foo=bar"),
            Some("abc-123".to_string())
        );
        assert_eq!(
            parse_navigation_conversation_id("https://chatgpt.com/"),
            None
        );
        assert_eq!(
            parse_navigation_conversation_id("https://example.com/c/x"),
            None
        );
    }
}
