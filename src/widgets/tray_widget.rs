use crate::{
    constants::{ERR_CREATE_RT, ERR_SEND_MSG_UI},
    structures::Align,
    ui,
    widget::HWidget,
};
use gtk::{
    traits::*, IconLookupFlags, IconTheme, Image, Menu, MenuBar, MenuItem, SeparatorMenuItem,
};
use std::{collections::HashMap, sync::Mutex, thread};
use stray::{
    message::{
        menu::{MenuType, TrayMenu},
        tray::{IconPixmap, StatusNotifierItem},
        NotifierItemCommand,
    },
    NotifierItemMessage, StatusNotifierWatcher,
};
use tokio::{runtime::Runtime, sync::mpsc};

struct NotifierItem {
    item: StatusNotifierItem,
    menu: Option<TrayMenu>,
}

pub struct StatusNotifierWrapper {
    menu: stray::message::menu::MenuItem,
}

lazy_static! {
    static ref STATE: Mutex<HashMap<String, NotifierItem>> = Mutex::new(HashMap::new());
}

// TODO: Document this code more and potentially clean it up.

impl StatusNotifierWrapper {
    /// Converts the content into a `MenuItem`
    fn into_menu_item(
        self,
        sender: mpsc::Sender<NotifierItemCommand>,
        notifier_address: String,
        menu_path: String,
    ) -> MenuItem {
        let item: Box<dyn AsRef<MenuItem>> = match self.menu.menu_type {
            MenuType::Separator => Box::new(SeparatorMenuItem::new()),
            MenuType::Standard => Box::new(MenuItem::with_label(self.menu.label.as_str())),
        };

        let item = (*item).as_ref().clone();

        {
            let sender = sender.clone();
            let notifier_address = notifier_address.clone();
            let menu_path = menu_path.clone();

            item.connect_activate(move |_item| {
                sender
                    .try_send(NotifierItemCommand::MenuItemClicked {
                        submenu_id: self.menu.id,
                        menu_path: menu_path.clone(),
                        notifier_address: notifier_address.clone(),
                    })
                    .unwrap();
            });
        };

        let submenu = Menu::new();
        if !self.menu.submenu.is_empty() {
            for submenu_item in self.menu.submenu.iter().cloned() {
                let submenu_item = StatusNotifierWrapper { menu: submenu_item };
                let submenu_item = submenu_item.into_menu_item(
                    sender.clone(),
                    notifier_address.clone(),
                    menu_path.clone(),
                );
                submenu.append(&submenu_item);
            }

            item.set_submenu(Some(&submenu));
        }

        item
    }
}

impl NotifierItem {
    /// Gets the icon for this tray item.
    fn get_icon(&self) -> Option<Image> {
        match &self.item.icon_pixmap {
            None => self.get_icon_from_theme(),
            Some(pixmaps) => self.get_icon_from_pixmaps(pixmaps),
        }
    }

    fn get_icon_from_pixmaps(&self, pixmaps: &[IconPixmap]) -> Option<Image> {
        let pixmap = pixmaps
            .iter()
            .find(|pm| pm.height > 20 && pm.height < 32)
            .expect("No icon of suitable size found");

        let pixbuf = gtk::gdk_pixbuf::Pixbuf::new(
            gtk::gdk_pixbuf::Colorspace::Rgb,
            true,
            8,
            pixmap.width,
            pixmap.height,
        )
        .expect("Failed to allocate pixbuf");

        for y in 0..pixmap.height {
            for x in 0..pixmap.width {
                let index = (y * pixmap.width + x) * 4;
                let a = pixmap.pixels[index as usize];
                let r = pixmap.pixels[(index + 1) as usize];
                let g = pixmap.pixels[(index + 2) as usize];
                let b = pixmap.pixels[(index + 3) as usize];
                pixbuf.put_pixel(x as u32, y as u32, r, g, b, a);
            }
        }

        Some(Image::from_pixbuf(Some(&pixbuf)))
    }

    /// Tries to get the application icon from the current icon theme.
    fn get_icon_from_theme(&self) -> Option<Image> {
        let theme = gtk::IconTheme::default().unwrap_or_else(IconTheme::new);
        theme.rescan_if_needed();

        if let Some(path) = self.item.icon_theme_path.as_ref() {
            theme.append_search_path(path);
        }

        let icon_name = self.item.icon_name.as_ref().unwrap();
        let icon = theme.lookup_icon(icon_name, 16, IconLookupFlags::GENERIC_FALLBACK);

        icon.map(|i| Image::from_pixbuf(i.load_icon().ok().as_ref()))
    }
}

/// Creates a new Tray widget.
pub struct TrayWidget;

// Implements HWidget for the widget so that we can actually use it.
impl HWidget for TrayWidget {
    fn add(
        self,
        name: &str,
        align: Align,
        left: &gtk::Box,
        centered: &gtk::Box,
        right: &gtk::Box,
        box_holder: Option<&gtk::Box>,
    ) {
        if !experimental!() {
            return;
        }

        let menu_bar = MenuBar::new();
        menu_bar.set_widget_name(name);
        ui::add_and_align(&menu_bar, align, left, centered, right, box_holder);
        let (sender, receiver) = mpsc::channel(32);
        let (cmd_tx, cmd_rx) = mpsc::channel(32);

        spawn_local_handler(menu_bar, receiver, cmd_tx);
        start_communication_thread(sender, cmd_rx);
    }
}

fn spawn_local_handler(
    v_box: MenuBar,
    mut receiver: mpsc::Receiver<NotifierItemMessage>,
    cmd_tx: mpsc::Sender<NotifierItemCommand>,
) {
    let main_context = glib::MainContext::default();
    let future = async move {
        while let Some(item) = receiver.recv().await {
            let mut state = STATE.lock().unwrap();

            match item {
                NotifierItemMessage::Update {
                    address: id,
                    item,
                    menu,
                } => {
                    state.insert(id, NotifierItem { item: *item, menu });
                }
                NotifierItemMessage::Remove { address } => {
                    state.remove(&address);
                }
            }

            for child in v_box.children() {
                v_box.remove(&child);
            }

            for (address, notifier_item) in state.iter() {
                if let Some(ref icon) = notifier_item.get_icon() {
                    // Create the menu

                    let menu_item = MenuItem::new();
                    let menu_item_box = gtk::Box::default();
                    menu_item_box.add(icon);
                    menu_item.add(&menu_item_box);

                    if let Some(tray_menu) = &notifier_item.menu {
                        let menu = Menu::new();
                        tray_menu
                            .submenus
                            .iter()
                            .map(|submenu| StatusNotifierWrapper {
                                menu: submenu.to_owned(),
                            })
                            .map(|item| {
                                let menu_path =
                                    notifier_item.item.menu.as_ref().unwrap().to_string();
                                let address = address.to_string();
                                item.into_menu_item(cmd_tx.clone(), address, menu_path)
                            })
                            .for_each(|item| menu.append(&item));

                        if !tray_menu.submenus.is_empty() {
                            // Has items in the sub menu.
                            menu_item.set_submenu(Some(&menu));
                        }
                    }
                    v_box.append(&menu_item);
                };

                v_box.show_all();
            }
        }
    };

    main_context.spawn_local(future);
}

fn start_communication_thread(
    sender: mpsc::Sender<NotifierItemMessage>,
    cmd_rx: mpsc::Receiver<NotifierItemCommand>,
) {
    thread::spawn(move || {
        let runtime = Runtime::new().expect(ERR_CREATE_RT);

        runtime.block_on(async {
            let tray = StatusNotifierWatcher::new(cmd_rx).await.unwrap();
            let mut host = tray.create_notifier_host("Hybrid").await.unwrap();

            while let Ok(message) = host.recv().await {
                sender.send(message).await.expect(ERR_SEND_MSG_UI);
            }

            host.destroy().await.unwrap();
        })
    });
}
