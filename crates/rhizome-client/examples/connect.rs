//! A minimal terminal IRC client built on the engine.
//!
//! ```text
//! cargo run -p rhizome-client --example connect -- \
//!     --server irc.libera.chat --nick my_nick --join "#rhizome"
//! ```
//!
//! To log in with a NickServ account, pass `--sasl-user NAME` and put the
//! password in the `RHIZOME_SASL_PASSWORD` environment variable. It is
//! deliberately not a command-line argument: those end up in shell history and
//! process listings.
//!
//! Type text to talk in the current channel. Commands:
//!
//! ```text
//! /join #chan        /part [#chan]       /me does something
//! /msg nick text     /nick newnick       /buf #chan  (switch channel)
//! /raw LINE          /quit [reason]
//! ```

use std::process::ExitCode;

use rhizome_client::{spawn, Config, Event, Handle, MessageKind};
use rhizome_proto::format;
use tokio::io::{AsyncBufReadExt, BufReader};

struct Args {
    server: String,
    port: Option<u16>,
    tls: bool,
    nick: String,
    realname: Option<String>,
    join: Vec<String>,
    sasl_user: Option<String>,
}

fn parse_args() -> Result<Args, String> {
    let mut args = Args {
        server: "irc.libera.chat".to_owned(),
        port: None,
        tls: true,
        nick: String::new(),
        realname: None,
        join: Vec::new(),
        sasl_user: None,
    };
    let mut it = std::env::args().skip(1);
    while let Some(flag) = it.next() {
        let mut value = |name: &str| it.next().ok_or_else(|| format!("{name} needs a value"));
        match flag.as_str() {
            "--server" => args.server = value("--server")?,
            "--port" => {
                args.port = Some(
                    value("--port")?
                        .parse()
                        .map_err(|_| "--port must be a number".to_owned())?,
                )
            }
            "--nick" => args.nick = value("--nick")?,
            "--realname" => args.realname = Some(value("--realname")?),
            "--join" => args
                .join
                .extend(value("--join")?.split(',').map(str::to_owned)),
            "--sasl-user" => args.sasl_user = Some(value("--sasl-user")?),
            "--no-tls" => args.tls = false,
            "-h" | "--help" => return Err(String::new()),
            other => return Err(format!("unknown option {other}")),
        }
    }
    if args.nick.is_empty() {
        return Err("--nick is required".to_owned());
    }
    Ok(args)
}

fn usage() {
    eprintln!(
        "usage: connect --nick NICK [--server HOST] [--port N] [--no-tls]\n\
         \x20              [--join #a,#b] [--realname TEXT] [--sasl-user ACCOUNT]\n\
         \n\
         SASL password: set RHIZOME_SASL_PASSWORD in the environment."
    );
}

/// `HH:MM` from a server-time stamp such as `2026-09-25T10:00:00.000Z`.
fn clock(time: Option<&str>) -> String {
    time.and_then(|t| t.get(11..16))
        .unwrap_or("     ")
        .to_owned()
}

fn show(event: &Event) {
    match event {
        Event::Connecting => println!("-- connecting"),
        Event::Connected => println!("-- connected, registering"),
        Event::Registered { nick } => println!("-- registered as {nick}"),
        Event::Network(name) => println!("-- network: {name}"),
        Event::Disconnected { reason, retry_in } => match retry_in {
            Some(d) => println!("-- disconnected ({reason}); retrying in {:.1}s", d.as_secs_f32()),
            None => println!("-- disconnected ({reason}); giving up"),
        },
        Event::Message(m) => {
            let text = format::strip(&m.text);
            let mark = if m.highlight { '!' } else { ' ' };
            let time = clock(m.time.as_deref());
            match m.kind {
                MessageKind::Action => println!("{time}{mark}{} * {} {text}", m.buffer, m.sender),
                MessageKind::Notice => println!("{time}{mark}{} -{}- {text}", m.buffer, m.sender),
                MessageKind::Privmsg => println!("{time}{mark}{} <{}> {text}", m.buffer, m.sender),
            }
        }
        Event::Server(text) => println!("   {}", format::strip(text)),
        Event::Ctcp { from, command, .. } => println!("-- CTCP {command} from {from}"),
        Event::Joined { channel } => println!("-- you joined {channel}"),
        Event::Parted { channel, .. } => println!("-- you left {channel}"),
        Event::Kicked { channel, by, reason } => {
            println!("-- kicked from {channel} by {by} ({})", reason.as_deref().unwrap_or(""))
        }
        Event::MemberJoined { channel, nick, .. } => println!("-- {nick} joined {channel}"),
        Event::MemberParted { channel, nick, .. } => println!("-- {nick} left {channel}"),
        Event::MemberKicked { channel, nick, by, .. } => {
            println!("-- {nick} was kicked from {channel} by {by}")
        }
        Event::MemberQuit { nick, reason, .. } => {
            println!("-- {nick} quit ({})", reason.as_deref().unwrap_or(""))
        }
        Event::NickChanged { old, new, .. } => println!("-- {old} is now {new}"),
        Event::Topic { channel, topic } => {
            println!("-- topic of {channel}: {}", topic.as_deref().map_or_else(String::new, format::strip))
        }
        Event::Names { channel, members } => {
            let list: Vec<String> = members
                .iter()
                .map(|m| format!("{}{}", m.top_prefix().map_or(String::new(), String::from), m.nick))
                .collect();
            println!("-- {channel}: {} members: {}", list.len(), list.join(" "));
        }
        Event::Mode { target, by, modes } => println!("-- {by} sets mode {modes} on {target}"),
        Event::Error { code, text } => println!("!! error {code}: {text}"),
        Event::ServerError(text) => println!("!! server: {text}"),
        Event::AuthFailed(reason) => println!("!! login failed: {reason}"),
    }
}

/// Prints a local message and reports success, so it can stand in for a command
/// that talks to the server.
fn note(text: &str) -> Result<(), rhizome_client::Closed> {
    println!("{text}");
    Ok(())
}

/// Handles one line typed by the user. Returns `false` to stop.
fn handle_input(line: &str, handle: &Handle, current: &mut Option<String>) -> bool {
    let line = line.trim_end();
    if line.is_empty() {
        return true;
    }
    let Some(command) = line.strip_prefix('/') else {
        match current {
            Some(buffer) => {
                let _ = handle.message(buffer, line);
            }
            None => println!("!! no channel selected; use /join #channel or /msg nick text"),
        }
        return true;
    };

    let (name, rest) = command.split_once(' ').unwrap_or((command, ""));
    let rest = rest.trim();
    let result = match name {
        "join" | "j" if !rest.is_empty() => {
            *current = Some(rest.split(',').next().unwrap_or(rest).to_owned());
            let channels: Vec<&str> = rest.split(',').collect();
            handle.join(&channels)
        }
        "part" | "leave" => {
            let target = if rest.is_empty() { current.clone() } else { Some(rest.to_owned()) };
            match target {
                Some(channel) => handle.part(&channel, None),
                None => note("!! part which channel?"),
            }
        }
        "me" if !rest.is_empty() => match current {
            Some(buffer) => handle.action(buffer, rest),
            None => note("!! no channel selected"),
        },
        "msg" | "query" => match rest.split_once(' ') {
            Some((target, text)) => {
                *current = Some(target.to_owned());
                handle.message(target, text)
            }
            None => note("!! usage: /msg nick text"),
        },
        "nick" if !rest.is_empty() => handle.nick(rest),
        "buf" | "b" if !rest.is_empty() => {
            *current = Some(rest.to_owned());
            note(&format!("-- talking in {rest}"))
        }
        "raw" | "quote" if !rest.is_empty() => handle.raw(rest),
        "quit" | "exit" => {
            let _ = handle.quit(if rest.is_empty() { None } else { Some(rest) });
            return false;
        }
        _ => note(&format!("!! unknown or incomplete command: /{command}")),
    };
    result.is_ok()
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> ExitCode {
    let args = match parse_args() {
        Ok(a) => a,
        Err(msg) => {
            if !msg.is_empty() {
                eprintln!("error: {msg}");
            }
            usage();
            return ExitCode::from(2);
        }
    };

    let mut config = Config::new(&args.server, &args.nick).autojoin(args.join.clone());
    if !args.tls {
        config = config.plaintext();
    }
    if let Some(port) = args.port {
        config = config.port(port);
    }
    if let Some(realname) = &args.realname {
        config = config.realname(realname);
    }
    if let Some(user) = &args.sasl_user {
        match std::env::var("RHIZOME_SASL_PASSWORD") {
            Ok(password) if !password.is_empty() => config = config.sasl_plain(user, password),
            _ => {
                eprintln!("error: --sasl-user needs the password in RHIZOME_SASL_PASSWORD");
                return ExitCode::from(2);
            }
        }
    }

    let mut client = spawn(config);
    let mut current = args.join.first().cloned();
    let mut stdin = BufReader::new(tokio::io::stdin()).lines();
    let mut input_open = true;
    let mut failed = false;

    loop {
        tokio::select! {
            event = client.events.recv() => {
                let Some(event) = event else { break };
                if matches!(event, Event::AuthFailed(_)) {
                    failed = true;
                }
                if matches!(event, Event::Disconnected { retry_in: None, .. }) {
                    show(&event);
                    break;
                }
                show(&event);
            }
            line = stdin.next_line(), if input_open => {
                match line {
                    Ok(Some(line)) => {
                        if !handle_input(&line, &client.handle, &mut current) {
                            // Keep draining events so the goodbye is visible.
                            input_open = false;
                        }
                    }
                    // End of input (Ctrl-D, or a closed pipe): leave politely.
                    _ => {
                        let _ = client.handle.quit(Some("Rhizome"));
                        input_open = false;
                    }
                }
            }
        }
    }

    client.wait().await;
    if failed {
        ExitCode::FAILURE
    } else {
        ExitCode::SUCCESS
    }
}
