mod model;
mod ui;

use aurora_i18n::tr;
use aurora_net::chat::{self, Command, Event};
use aurora_net::login::LoginError;
use aurora_net::social::GroupMembership;
use crossterm::event::{Event as TerminalEvent, EventStream, KeyCode, KeyEventKind, KeyModifiers};
use futures_util::StreamExt;
use model::{Action, Model};
use std::io;
use tokio::sync::mpsc;
use tokio::task::JoinSet;
use uuid::Uuid;

#[derive(Default)]
struct Options {
    demo: bool,
    help: bool,
    version: bool,
    language: String,
}

fn options(args: impl Iterator<Item = String>) -> Result<Options, String> {
    let mut options = Options::default();
    for argument in args {
        match argument.as_str() {
            "--demo" => options.demo = true,
            "--help" | "-h" => options.help = true,
            "--version" | "-V" => options.version = true,
            _ if argument.starts_with("--language=") => {
                let language = argument.trim_start_matches("--language=");
                if !matches!(language, "en" | "fr" | "ja") {
                    return Err(tr!("tui-language-error").into());
                }
                options.language = language.into();
            }
            _ => return Err(tr!("tui-argument-error").into()),
        }
    }
    Ok(options)
}

#[tokio::main(flavor = "current_thread")]
async fn main() {
    aurora_i18n::select("", None);
    let result = match options(std::env::args().skip(1)) {
        Ok(options) => {
            aurora_i18n::select(&options.language, None);
            if options.help {
                println!("{}", tr!("tui-cli-help"));
                return;
            }
            if options.version {
                println!("Aurora Chat {}", aurora_version::version());
                return;
            }
            run(options).await
        }
        Err(error) => Err(io::Error::other(error)),
    };
    if let Err(error) = result {
        eprintln!("{}", tr!("tui-error", reason = error.to_string()));
        std::process::exit(1);
    }
}

fn demo_groups() -> Vec<GroupMembership> {
    vec![GroupMembership {
        id: Uuid::from_u128(1),
        name: tr!("tui-demo-group").into(),
        insignia: Uuid::nil(),
        powers: 0,
        accept_notices: false,
        list_in_profile: true,
        contribution: 0,
    }]
}

fn demo_command(model: &mut Model, command: Command) {
    match command {
        Command::Join(id) => model.event(Event::Joined(id)),
        Command::Leave(id) => model.event(Event::Left(id)),
        Command::Group { id, text } => model.event(Event::Group {
            id,
            from: tr!("tui-demo-you").into(),
            text,
        }),
        Command::Local { text, .. } => model.event(Event::Local {
            from: tr!("tui-demo-you").into(),
            text,
        }),
        Command::Logout => {}
    }
}

async fn run(options: Options) -> io::Result<()> {
    let old_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        ui::restore();
        old_hook(info);
    }));
    let mut screen = ui::Screen::new()?;
    let mut input = EventStream::new();
    let mut model = Model::default();
    let mut login = ui::Login::default();
    let mut show_login = !options.demo;
    let mut status = if options.demo { tr!("tui-demo") } else { tr!("tui-offline") }.to_owned();
    let (events_tx, mut events_rx) = mpsc::channel(128);
    let mut commands = None::<mpsc::Sender<Command>>;
    let mut sessions = JoinSet::new();
    let mut quitting = false;
    let mut dirty = true;
    if options.demo {
        model.event(Event::Groups(demo_groups()));
        model.notice(tr!("tui-demo-intro"));
        model.notice(tr!("tui-welcome"));
    }
    loop {
        if dirty {
            screen.draw(&model, &login, show_login, &status)?;
            dirty = false;
        }
        tokio::select! {
            event = input.next() => {
                let Some(event) = event else { break; };
                let event = event?;
                match event {
                    TerminalEvent::Key(key) if key.kind != KeyEventKind::Release => {
                        let quit = (key.code == KeyCode::Char('c') && key.modifiers.contains(KeyModifiers::CONTROL))
                            || (key.code == KeyCode::Esc && login.approval.is_none());
                        if quit && !quitting {
                            quitting = true;
                        } else if quitting {
                            continue;
                        } else if show_login {
                            if key.code == KeyCode::Enter && sessions.is_empty() && login.approval.is_none() {
                                if let Some(request) = login.request() {
                                    while events_rx.try_recv().is_ok() {}
                                    model = Model::default();
                                    let (tx, rx) = mpsc::channel(64);
                                    commands = Some(tx);
                                    let events = events_tx.clone();
                                    sessions.spawn(chat::run(request, events, rx));
                                    status = tr!("tui-connecting").into();
                                }
                            } else if sessions.is_empty() {
                                login.key(key);
                            }
                        } else {
                            match key.code {
                                KeyCode::Tab => model.switch(1),
                                KeyCode::BackTab => model.switch(-1),
                                KeyCode::PageUp => model.active_mut().scroll = model.active().scroll.saturating_add(8),
                                KeyCode::PageDown => model.active_mut().scroll = model.active().scroll.saturating_sub(8),
                                KeyCode::Enter => match model.submit() {
                                    Action::Quit => quitting = true,
                                    Action::Send(command) => {
                                        if options.demo {
                                            demo_command(&mut model, command);
                                        } else if let Some(tx) = &commands {
                                            match tx.try_send(command) {
                                                Ok(()) => {}
                                                Err(_) => model.notice(tr!("tui-send-busy")),
                                            }
                                        } else {
                                            model.notice(tr!("tui-offline"));
                                        }
                                    }
                                    Action::None => {}
                                },
                                _ => ui::edit(&mut model.active_mut().draft, key),
                            }
                        }
                        dirty = true;
                    }
                    TerminalEvent::Paste(text) if !quitting => {
                        if show_login && sessions.is_empty() && login.approval.is_none() {
                            login.fields[login.selected].insert(&text);
                        } else if !show_login {
                            model.active_mut().draft.insert(&text);
                        }
                        dirty = true;
                    }
                    TerminalEvent::Resize(_, _) => dirty = true,
                    _ => {}
                }
                if quitting {
                    if let Some(tx) = commands.take() {
                        tx.send(Command::Logout).await.map_err(|_| io::Error::other(tr!("tui-offline")))?;
                        status = tr!("tui-quitting").into();
                        dirty = true;
                    } else if sessions.is_empty() {
                        break;
                    }
                }
            }
            Some(event) = events_rx.recv() => {
                if matches!(event, Event::Connected(_)) && commands.is_some() && !quitting {
                    show_login = false;
                    status = tr!("tui-online").into();
                    model.notice(tr!("tui-welcome"));
                }
                model.event(event);
                dirty = true;
            }
            Some(result) = sessions.join_next(), if !sessions.is_empty() => {
                commands = None;
                let result = result.map_err(io::Error::other)?;
                if quitting {
                    return result.map_err(io::Error::other);
                }
                status = tr!("tui-offline").into();
                match result {
                    Ok(()) => {
                        model.notice(tr!("tui-offline"));
                        show_login = true;
                    }
                    Err(chat::Error::Login(error)) => {
                        login.scroll = 0;
                        login.message = error.to_string();
                        login.approval = match error {
                            LoginError::TosRequired(message) => {
                                login.message = message;
                                Some(ui::Approval::Tos)
                            }
                            LoginError::CriticalMessage(message) => {
                                login.message = message;
                                Some(ui::Approval::Critical)
                            }
                            _ => None,
                        };
                        show_login = true;
                    }
                    Err(error) => {
                        let message = tr!("tui-error", reason = error.to_string());
                        model.notice(&message);
                        login.message = message;
                        show_login = true;
                    }
                }
                dirty = true;
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn credentials_are_not_accepted_as_command_line_arguments() {
        for args in [
            vec!["--password=test"],
            vec!["--username=test"],
            vec!["--login-uri=http://example.invalid"],
            vec!["--language=no"],
        ] {
            assert!(options(args.into_iter().map(String::from)).is_err());
        }
        let options = options(["--demo", "--language=ja"].into_iter().map(String::from)).unwrap();
        assert!(options.demo);
        assert_eq!(options.language, "ja");
    }
}
