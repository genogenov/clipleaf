use std::{cell::Cell, rc::Rc};

use clipleaf_default_gui::GuiCommand;
use clipleaf_ipc::protocol::v1::models::ListReply;
use gtk4::{
    gdk::{self, prelude::SurfaceExt},
    gio::{self, prelude::ApplicationExtManual},
    glib::{self, object::ObjectExt},
    prelude::{BoxExt, GestureSingleExt, GtkWindowExt, NativeExt, WidgetExt},
};
use gtk4_layer_shell::{Edge, KeyboardMode, Layer, LayerShell};

use crate::{daemon_client::ClientCommand, pubsub::Pub, ui::item_list::ItemList};

pub struct MainWindow {
    win: gtk4::ApplicationWindow,
    _hold: gio::ApplicationHoldGuard,
    warming: Cell<bool>,
    pending_cmd: Cell<Option<GuiCommand>>,
    client_sender: Pub<ClientCommand>,
    status: gtk4::Box,
    status_tooltip: gtk4::Label,
    item_list: ItemList,
    offline_label: gtk4::Label,
}

impl MainWindow {
    pub fn new(app: &gtk4::Application, client_sender: Pub<ClientCommand>) -> Rc<Self> {
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

        let (status, status_tooltip) = build_status_elements();
        let header = gtk4::Box::new(gtk4::Orientation::Horizontal, 0);
        header.add_css_class("clipleaf-header");
        header.append(&status);

        let card = gtk4::Box::new(gtk4::Orientation::Vertical, 4);
        card.add_css_class("clipleaf-main-card");
        card.set_halign(gtk4::Align::Center);
        card.set_valign(gtk4::Align::Center);
        win.set_child(Some(&card));

        card.append(&header);

        let item_list = ItemList::new();
        let offline_label = gtk4::Label::builder()
            .label("Connecting to daemon...")
            .css_classes(["clipleaf-offline"])
            .vexpand(true)
            .build();
        card.append(&item_list.root);
        card.append(&offline_label);
        item_list.root.set_visible(false);

        let main = Rc::new(Self {
            win: win.clone(),
            _hold: app.hold(),
            warming: Cell::new(false),
            pending_cmd: Cell::new(None),
            client_sender,
            status,
            status_tooltip,
            item_list,
            offline_label,
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

    pub fn set_status(&self, text: &str, is_connected: bool) {
        self.status_tooltip.set_label(text);
        self.status
            .set_css_classes(&["clipleaf-connection-status", if is_connected { "connected" } else { "disconnected" }]);
        self.item_list.root.set_visible(is_connected);
        self.offline_label.set_visible(!is_connected);
        if !is_connected {
            self.offline_label.set_label(text);
            self.win.set_focus(None::<&gtk4::Widget>);
        }
    }

    pub fn set_list(&self, list: ListReply) {
        self.item_list.set_list(list);
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
        if self.item_list.root.is_visible() {
            self.item_list.focus();
        }
        let _ = self.client_sender.try_publish(ClientCommand::SetVisible);
    }
    pub fn hide(&self) {
        self.win.set_visible(false);
        let _ = self.client_sender.try_publish(ClientCommand::SetHidden);
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

fn build_status_elements() -> (gtk4::Box, gtk4::Label) {
    let status = gtk4::Box::builder()
        .orientation(gtk4::Orientation::Horizontal)
        .spacing(0)
        .css_classes(["clipleaf-connection-status", "connecting"])
        .hexpand(true)
        .halign(gtk4::Align::End)
        .valign(gtk4::Align::Center)
        .has_tooltip(true)
        .build();
    let tip = gtk4::Label::builder()
        .label("Connecting to daemon...")
        .css_classes(["clipleaf-connection-status-tooltip"])
        .build();
    let t = tip.clone();
    status.connect_query_tooltip(move |_, _x, _y, _keyboard, tooltip| {
        tooltip.set_custom(Some(&t)); // our widget instead of GTK's default label
        true // true = show the tooltip
    });

    (status, tip)
}
