mod args;
mod daemon_client;
mod instance;
mod main_window;
mod pubsub;
mod state;
mod style;

use std::{
    cell::Cell,
    env,
    io::{self, Write},
    process::ExitCode,
};

const DEFAULT_GDK_RENDERER: &str = "gl";
const SIGTERM: i32 = 15;
const SIGINT: i32 = 2;

use clipleaf_core::{init_logging, log_debug, log_error};
use clipleaf_default_gui::GuiCommand;
use gtk4::{
    Application, gdk,
    gio::{
        ApplicationFlags,
        prelude::{ApplicationExt, ApplicationExtManual},
    },
};

use crate::{
    args::{ArgCommand, StartState},
    daemon_client::spawn_client_worker,
    instance::Instance,
    main_window::MainWindow,
    state::{AppState, ConnState},
};

const EXIT_USAGE_ERROR: u8 = 2;

pub fn main() -> ExitCode {
    init_logging();
    let args = match args::parse_args(env::args_os()) {
        Ok(ArgCommand::Help(usage)) => {
            let _ = io::stdout().write_all(usage.as_bytes());
            return ExitCode::SUCCESS;
        }
        Ok(ArgCommand::Cmd(args)) => args,
        Err(e) => {
            log_error!("{e}");
            return ExitCode::from(EXIT_USAGE_ERROR);
        }
    };
    log_debug!("Args: {args:?}");

    match run(args) {
        Ok(exit) => ExitCode::from(exit),
        Err(err) => {
            log_error!("{}", err);
            ExitCode::FAILURE
        }
    }
}

fn run(start: StartState) -> io::Result<gtk4::glib::ExitCode> {
    let Some((_lock, listener, _socket_path)) = Instance::acquire()? else {
        log_debug!("Another GUI instance already running.");
        return Ok(gtk4::glib::ExitCode::SUCCESS);
    };
    let app = build_app();
    app.connect_activate(|_| {});

    let (gui_cmd_sender, gui_cmd_sub) = pubsub::channel(32)?;
    let sub = Cell::new(Some(gui_cmd_sub));
    let (client_sender, client_sub) = pubsub::channel(32)?;
    let (daemon_client, ev_sub) = daemon_client::DaemonClient::new(client_sub)?;
    let ev_sub = Cell::new(Some(ev_sub));
    app.connect_startup(move |app| {
        let clone1 = app.clone();
        glib_unix::unix_signal_add_local(SIGTERM, move || {
            clone1.quit();
            gdk::glib::ControlFlow::Break
        });
        let clone2 = app.clone();
        glib_unix::unix_signal_add_local(SIGINT, move || {
            clone2.quit();
            gdk::glib::ControlFlow::Break
        });

        match gdk::Display::default() {
            Some(display) => style::load(&display),
            None => log_error!("No default display, CSS not loaded"),
        }
        let main = MainWindow::new(app, client_sender.clone());
        let mut state = AppState {
            con_id: 0,
            conn: ConnState::Connecting
        };
        main.warmup();
        if start == StartState::Shown {
            main.apply(GuiCommand::Show);
        }
        let main_copy = main.clone();
        if let Some(sub) = sub.take() {
            sub.subscribe(move |cmd| main_copy.as_ref().apply(cmd));
        }
        if let Some(ev_sub) = ev_sub.take() {
            ev_sub.subscribe(move |ev| {
                state.apply(ev, &main);
            });
        }
    });
    Instance::listen(listener, gui_cmd_sender)?;
    spawn_client_worker(daemon_client)?;

    Ok(app.run_with_args::<&str>(&[]))
}

fn build_app() -> Application {
    // if env::var_os("GTK_DEBUG").is_none() {
    //     unsafe { env::set_var("GTK_DEBUG", "interactive") };
    // }
    if env::var_os("GSK_RENDERER").is_none() {
        unsafe { env::set_var("GSK_RENDERER", DEFAULT_GDK_RENDERER) };
    }

    gdk::set_allowed_backends("wayland");
    gtk4::Application::builder()
        .application_id("io.github.genogenov.clipleaf.DefaultGui")
        .flags(ApplicationFlags::NON_UNIQUE)
        .build()
}
