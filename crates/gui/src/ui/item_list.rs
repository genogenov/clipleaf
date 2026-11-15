use clipleaf_ipc::protocol::v1::models::EntrySummary;
use gtk4::{ListView, ScrolledWindow, SignalListItemFactory, SingleSelection, gio::{self, prelude::ListModelExt}, glib::{self, object::Cast}, prelude::ListItemExt};

use crate::ui::row_widgets::RowWidgets;

pub struct ItemList {
    item_list: gio::ListStore,
    pub list_window: ScrolledWindow,
    _list_view: ListView,
    _selection: SingleSelection,
    _factory: SignalListItemFactory
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
            list_item.connect_item_notify(move |list_item|{
                match list_item.item(){
                    Some(obj) => {
                        let Some(boxed) = obj.downcast_ref::<glib::BoxedAnyObject>() else {
                            return;
                        };
                        let entry = boxed.borrow::<EntrySummary>();
                        row.fill(&entry)},
                    None => row.clear(),
                }
            });
        });

        let list_window = ScrolledWindow::builder()
            .hscrollbar_policy(gtk4::PolicyType::Never)
            .vexpand(true)
            .css_classes(["clipleaf-item-list-window"])
            .child(&list_view)
            .build();

        Self {
            item_list,
            list_window,
            _list_view: list_view,
            _selection: selection,
            _factory: factory
        }
    }

    pub fn set_list(&self, entires: Vec<EntrySummary>){
        let objs: Vec<glib::BoxedAnyObject> = entires.into_iter().map(glib::BoxedAnyObject::new).collect();
        self.item_list.splice(0, self.item_list.n_items(), &objs);
    }

    pub fn clear_list(&self){
        self.item_list.remove_all();
    }
}
