use std::{
    cell::RefCell,
    env,
    io::{self, Write},
    process::ExitCode,
};

const DEFAULT_GDK_RENDERER: &str = "gl";

use clipleaf_core::{init_logging, log_debug, log_error};
use gtk4::{
    Application, gdk,
    gio::{
        ApplicationFlags,
        prelude::{ApplicationExt, ApplicationExtManual},
    },
};

use crate::{
    args::{ArgCommand, StartState},
    instance::Instance,
    main_window::MainWindow,
};

mod args;
mod instance;
mod main_window;

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
    let Some(_instance) = Instance::ackuire()? else {
        log_debug!("Another GUI instance already running.");
        return Ok(gtk4::glib::ExitCode::SUCCESS);
    };
    let app = build_app();
    app.connect_activate(|_| {});
    let window = RefCell::new(None);
    app.connect_startup(move |app| {
        load_css();
        let main = MainWindow::new(&app);
        if start == StartState::Shown {
            main.show();
        }
        window.replace(Some(main));
    });
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

fn load_css() {
    let Some(display) = gdk::Display::default() else {
        log_error!("no default display, CSS not loaded");
        return;
    };
    let css = gtk4::CssProvider::new();
    css.connect_parsing_error(|_, section, err| log_error!("css {}: {}", section.to_str(), err));
    css.load_from_string(include_str!("../assets/default.css"));
    gtk4::style_context_add_provider_for_display(&display, &css, gtk4::STYLE_PROVIDER_PRIORITY_APPLICATION);
}
