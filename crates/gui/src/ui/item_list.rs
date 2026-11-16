use clipleaf_ipc::protocol::v1::models::EntrySummary;
use gtk4::{
    ListScrollFlags, ListView, ScrolledWindow, SignalListItemFactory, SingleSelection,
    gio::{self, prelude::ListModelExt},
    glib::{self, object::Cast},
    prelude::{BoxExt, ListItemExt, RootExt, WidgetExt},
};

use crate::ui::row_widgets::RowWidgets;

pub struct ItemList {
    item_list: gio::ListStore,
    list_window: ScrolledWindow,
    empty_label: gtk4::Label,
    list_view: ListView,
    pub root: gtk4::Box,
    _selection: SingleSelection,
    _factory: SignalListItemFactory,
}

impl ItemList {
    pub fn new() -> Self {
        let item_list = gio::ListStore::new::<glib::BoxedAnyObject>();
        let factory = SignalListItemFactory::new();
        let selection = SingleSelection::new(Some(item_list.clone()));
        let list_view = ListView::builder()
            .model(&selection)
            .factory(&factory)
            .css_classes(["clipleaf-item-list"])
            .build();

        factory.connect_setup(|_, obj| {
            let Some(list_item) = obj.downcast_ref::<gtk4::ListItem>() else { return };
            let row = RowWidgets::new();
            list_item.set_child(Some(&row.root));
            list_item.connect_item_notify(move |list_item| match list_item.item() {
                Some(obj) => {
                    let Some(boxed) = obj.downcast_ref::<glib::BoxedAnyObject>() else {
                        return;
                    };
                    let entry = boxed.borrow::<EntrySummary>();
                    row.fill(&entry)
                }
                None => row.clear(),
            });
        });

        let empty_label = gtk4::Label::builder()
            .css_classes(["clipleaf-item-list-empty-label"])
            .label("No history yet")
            .visible(false)
            .vexpand(true)
            .hexpand(true)
            .build();

        let list_window = ScrolledWindow::builder()
            .hscrollbar_policy(gtk4::PolicyType::Never)
            .vexpand(true)
            .css_classes(["clipleaf-item-list-window"])
            .child(&list_view)
            .visible(false)
            .build();

        let root = gtk4::Box::builder()
            .css_classes(["clipleaf-item-list-root"])
            .orientation(gtk4::Orientation::Vertical)
            .hexpand(true)
            .vexpand(true)
            .build();

        root.append(&list_window);
        root.append(&empty_label);

        Self {
            item_list,
            list_window,
            list_view,
            _selection: selection,
            _factory: factory,
            empty_label,
            root,
        }
    }

    pub fn set_list(&self, entires: Vec<EntrySummary>) {
        let objs: Vec<glib::BoxedAnyObject> = entires.into_iter().map(glib::BoxedAnyObject::new).collect();
        self.item_list.splice(0, self.item_list.n_items(), &objs);
        if objs.is_empty() {
            self.show_empty(true);
        } else {
            self.show_empty(false);
            self.list_view.scroll_to(0, ListScrollFlags::FOCUS | ListScrollFlags::SELECT, None);
        }
    }

    pub fn focus(&self) {
        if self.list_window.is_visible() {
            self.list_view.grab_focus();
        }
    }

    fn show_empty(&self, empty: bool) {
        self.list_window.set_visible(!empty);
        self.empty_label.set_visible(empty);
        if empty && let Some(root) = self.root.root() {
            root.set_focus(None::<&gtk4::Widget>);
        } else if !empty {
            self.focus();
        }
    }
}
