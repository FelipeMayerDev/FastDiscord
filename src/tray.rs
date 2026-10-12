//! Native tray: StatusNotifier/D-Bus on Linux, tray-icon on Windows.
use crate::app::TrayCommand;
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
    mpsc::Sender,
};

pub struct Tray {
    available: Arc<AtomicBool>,
    #[cfg(target_os = "linux")]
    handle: ksni::Handle<LinuxTray>,
    #[cfg(windows)]
    _icon: tray_icon::TrayIcon,
}

impl Tray {
    pub fn is_available(&self) -> bool {
        #[cfg(target_os = "linux")]
        if self.handle.is_closed() {
            return false;
        }
        self.available.load(Ordering::Relaxed)
    }
}

#[cfg(target_os = "linux")]
impl Drop for Tray {
    fn drop(&mut self) {
        self.handle.shutdown();
    }
}

fn send(tx: &Sender<TrayCommand>, ctx: &egui::Context, command: TrayCommand) {
    let _ = tx.send(command);
    ctx.request_repaint();
}

#[cfg(target_os = "linux")]
struct LinuxTray {
    tx: Sender<TrayCommand>,
    ctx: egui::Context,
    available: Arc<AtomicBool>,
    icon: ksni::Icon,
}

#[cfg(target_os = "linux")]
impl ksni::Tray for LinuxTray {
    fn id(&self) -> String {
        "fastdiscord".into()
    }
    fn title(&self) -> String {
        "FastDiscord".into()
    }
    fn icon_pixmap(&self) -> Vec<ksni::Icon> {
        vec![self.icon.clone()]
    }
    fn activate(&mut self, _: i32, _: i32) {
        send(&self.tx, &self.ctx, TrayCommand::Show);
    }
    fn watcher_offline(&self, _: ksni::OfflineReason) -> bool {
        self.available.store(false, Ordering::Relaxed);
        // Bring the window back if the desktop's tray disappears.
        send(&self.tx, &self.ctx, TrayCommand::Show);
        true
    }
    fn watcher_online(&self) {
        self.available.store(true, Ordering::Relaxed);
        self.ctx.request_repaint();
    }
    fn menu(&self) -> Vec<ksni::MenuItem<Self>> {
        vec![
            ksni::menu::StandardItem {
                label: "Abrir FastDiscord".into(),
                activate: Box::new(|this: &mut Self| send(&this.tx, &this.ctx, TrayCommand::Show)),
                ..Default::default()
            }
            .into(),
            ksni::menu::SubMenu {
                label: "Status".into(),
                submenu: crate::settings::STATUSES
                    .iter()
                    .map(|&(status, label)| {
                        ksni::menu::StandardItem {
                            label: label.into(),
                            activate: Box::new(move |this: &mut Self| {
                                send(&this.tx, &this.ctx, TrayCommand::Status(status))
                            }),
                            ..Default::default()
                        }
                        .into()
                    })
                    .collect(),
                ..Default::default()
            }
            .into(),
            ksni::MenuItem::Separator,
            ksni::menu::StandardItem {
                label: "Sair do FastDiscord".into(),
                activate: Box::new(|this: &mut Self| send(&this.tx, &this.ctx, TrayCommand::Quit)),
                ..Default::default()
            }
            .into(),
        ]
    }
}

#[cfg(target_os = "linux")]
pub fn install(
    tx: Sender<TrayCommand>,
    ctx: egui::Context,
    runtime: &tokio::runtime::Handle,
) -> anyhow::Result<Tray> {
    use ksni::TrayMethods;
    let mut icon = crate::window::load_icon()
        .ok_or_else(|| anyhow::anyhow!("ícone da bandeja indisponível"))?;
    for pixel in icon.rgba.chunks_exact_mut(4) {
        pixel.rotate_right(1);
    }
    let available = Arc::new(AtomicBool::new(true));
    let service = LinuxTray {
        tx,
        ctx,
        available: available.clone(),
        icon: ksni::Icon {
            width: icon.width as i32,
            height: icon.height as i32,
            data: icon.rgba,
        },
    };
    let handle = runtime.block_on(async {
        tokio::time::timeout(std::time::Duration::from_secs(3), service.spawn()).await
    })??;
    Ok(Tray { available, handle })
}

#[cfg(windows)]
pub fn install(
    tx: Sender<TrayCommand>,
    ctx: egui::Context,
    _: &tokio::runtime::Handle,
) -> anyhow::Result<Tray> {
    use tray_icon::menu::{Menu, MenuEvent, MenuItem, Submenu};
    use tray_icon::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};
    let menu = Menu::new();
    // Stable IDs keep the registered callbacks valid after toggling the tray.
    let show = MenuItem::with_id("fastdiscord-show", "Abrir FastDiscord", true, None);
    let quit = MenuItem::with_id("fastdiscord-quit", "Sair do FastDiscord", true, None);
    let statuses: Vec<MenuItem> = crate::settings::STATUSES
        .iter()
        .map(|(status, label)| {
            MenuItem::with_id(format!("fastdiscord-status-{status}"), label, true, None)
        })
        .collect();
    let status_items: Vec<&dyn tray_icon::menu::IsMenuItem> = statuses
        .iter()
        .map(|item| item as &dyn tray_icon::menu::IsMenuItem)
        .collect();
    let status = Submenu::with_items("Status", true, &status_items)?;
    menu.append_items(&[&show, &status, &quit])?;
    let icon = TrayIconBuilder::new()
        .with_id("fastdiscord-tray")
        .with_tooltip("FastDiscord")
        .with_menu(Box::new(menu))
        .with_menu_on_left_click(false)
        .with_icon(
            crate::window::load_tray_icon()
                .ok_or_else(|| anyhow::anyhow!("ícone da bandeja indisponível"))?,
        )
        .build()?;
    let show_id = show.id().clone();
    let quit_id = quit.id().clone();
    let menu_tx = tx.clone();
    let menu_ctx = ctx.clone();
    MenuEvent::set_event_handler(Some(move |event: MenuEvent| {
        if event.id == show_id {
            send(&menu_tx, &menu_ctx, TrayCommand::Show);
        } else if event.id == quit_id {
            send(&menu_tx, &menu_ctx, TrayCommand::Quit);
        } else if let Some(&(status, _)) = crate::settings::STATUSES
            .iter()
            .find(|(status, _)| event.id.0 == format!("fastdiscord-status-{status}"))
        {
            send(&menu_tx, &menu_ctx, TrayCommand::Status(status));
        }
    }));
    let tray_id = icon.id().clone();
    TrayIconEvent::set_event_handler(Some(move |event: TrayIconEvent| {
        if matches!(event, TrayIconEvent::Click { id, button: MouseButton::Left, button_state: MouseButtonState::Up, .. } if id == tray_id)
        {
            send(&tx, &ctx, TrayCommand::Show);
        }
    }));
    Ok(Tray {
        available: Arc::new(AtomicBool::new(true)),
        _icon: icon,
    })
}

#[cfg(not(any(target_os = "linux", windows)))]
pub fn install(
    _: Sender<TrayCommand>,
    _: egui::Context,
    _: &tokio::runtime::Handle,
) -> anyhow::Result<Tray> {
    anyhow::bail!("bandeja não suportada nesta plataforma")
}

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use super::*;
    use ksni::Tray as _;

    #[test]
    fn lost_tray_restores_window_and_recovery_reenables_hiding() {
        let (tx, rx) = std::sync::mpsc::channel();
        let available = Arc::new(AtomicBool::new(true));
        let mut tray = LinuxTray {
            tx,
            ctx: egui::Context::default(),
            available: available.clone(),
            icon: ksni::Icon {
                width: 1,
                height: 1,
                data: vec![255, 1, 2, 3],
            },
        };
        tray.activate(0, 0);
        assert!(matches!(rx.try_recv().unwrap(), TrayCommand::Show));
        assert!(tray.watcher_offline(ksni::OfflineReason::No));
        assert!(!available.load(Ordering::Relaxed));
        assert!(matches!(rx.try_recv().unwrap(), TrayCommand::Show));
        tray.watcher_online();
        assert!(available.load(Ordering::Relaxed));
    }
}
