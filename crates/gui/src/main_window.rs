use std::{cell::Cell, rc::Rc};

use clipleaf_default_gui::GuiCommand;
use gtk4::{
    gdk::{self, prelude::SurfaceExt},
    gio::{self, prelude::ApplicationExtManual},
    glib::{self, object::ObjectExt},
    prelude::{BoxExt, GestureSingleExt, GtkWindowExt, NativeExt, WidgetExt},
};
use gtk4_layer_shell::{Edge, KeyboardMode, Layer, LayerShell};

pub struct MainWindow {
    win: gtk4::ApplicationWindow,
    _hold: gio::ApplicationHoldGuard,
    warming: Cell<bool>,
    pending_cmd: Cell<Option<GuiCommand>>,
}

#[allow(dead_code)]
impl MainWindow {
    pub fn new(app: &gtk4::Application) -> Rc<Self> {
        let win = gtk4::ApplicationWindow::new(app);
        win.init_layer_shell(); // before anything realizes it
        win.set_layer(Layer::Overlay);
        win.set_keyboard_mode(KeyboardMode::Exclusive);
        win.set_namespace(Some("clipleaf-default-gui"));

        for edge in [Edge::Left, Edge::Right, Edge::Top, Edge::Bottom] {
            win.set_anchor(edge, true); // full-screen backdrop
        }
        win.set_exclusive_zone(-1);
        win.add_css_class("clipleaf-backdrop");

        let status = gtk4::Label::new(Some("connecting…"));
        status.set_xalign(0.0);
        status.add_css_class("status");
        let card = gtk4::Box::new(gtk4::Orientation::Vertical, 4); // the visible popup
        card.add_css_class("clipleaf-main-card");
        card.set_halign(gtk4::Align::Center);
        card.set_valign(gtk4::Align::Center);
        card.append(&status);
        win.set_child(Some(&card));

        let main = Rc::new(Self {
            win: win.clone(),
            _hold: app.hold(),
            warming: Cell::new(false),
            pending_cmd: Cell::new(None),
        });

        let keys = gtk4::EventControllerKey::new();
        let p = main.clone();
        keys.connect_key_pressed(move |_, key, _, _| {
            if key == gdk::Key::Escape {
                p.hide();
                glib::Propagation::Stop
            } else {
                glib::Propagation::Proceed
            }
        });
        win.add_controller(keys);

        // click outside the card = a click on our own transparent backdrop → hide
        let click = gtk4::GestureClick::new();
        let p = main.clone();
        click.set_button(0);
        click.connect_pressed(move |_, _n_press, x, y| {
            let inside = p
                .win
                .pick(x, y, gtk4::PickFlags::DEFAULT)
                .is_some_and(|w| w == card || w.is_ancestor(&card));
            if !inside {
                p.hide();
            }
        });
        win.add_controller(click);

        main
    }
    pub fn warmup(self: &Rc<Self>) {
        self.warming.set(true);
        self.win.set_keyboard_mode(KeyboardMode::None);
        self.win.set_opacity(0.0);
        self.win.present();
        let Some(surface) = self.win.surface() else {
            return self.finish_warmup();
        };
        let id = Rc::new(Cell::new(None::<glib::SignalHandlerId>));
        let (window_clone, id_clone) = (self.clone(), id.clone());
        id.set(Some(surface.frame_clock().connect_after_paint(move |clock| {
            if let Some(id) = id_clone.take() {
                clock.disconnect(id);
            }
            window_clone.finish_warmup();
        })));
    }
    fn finish_warmup(&self) {
        self.warming.set(false);
        self.win.set_visible(false);
        self.win.set_opacity(1.0);
        self.win.set_keyboard_mode(KeyboardMode::Exclusive);
        if let Some(cmd) = self.pending_cmd.take() {
            self.apply(cmd);
        }
    }
    pub fn show(&self) {
        self.win.present();
    }
    pub fn hide(&self) {
        self.win.set_visible(false);
    }
    pub fn toggle(&self) {
        if self.win.is_visible() { self.hide() } else { self.show() }
    }
    pub fn apply(&self, cmd: GuiCommand) {
        if self.warming.get() {
            self.pending_cmd.set(Some(cmd));
            return;
        }
        match cmd {
            GuiCommand::Show => self.show(),
            GuiCommand::Hide => self.hide(),
            GuiCommand::Toggle => self.toggle(),
        }
    }
}
