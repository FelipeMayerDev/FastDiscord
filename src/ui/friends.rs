//! Friends and friend requests, backed by Discord relationships.
use crate::{app::VesktopApp, theme, util};
use egui::RichText;

#[derive(Clone, Copy, Default, PartialEq, Eq)]
pub enum HomePage {
    #[default]
    DirectMessages,
    Friends,
    Pending,
    NewConversation,
}

pub fn paint(app: &mut VesktopApp, ui: &mut egui::Ui) {
    let pending = app.home_page == HomePage::Pending;
    let new_dm = app.home_page == HomePage::NewConversation;
    ui.heading(if pending {
        "Solicitações de amizade"
    } else if new_dm {
        "Nova conversa"
    } else {
        "Amigos"
    });
    ui.separator();
    if ui
        .add_enabled(!app.relationships_loading, egui::Button::new("Atualizar"))
        .clicked()
    {
        app.refresh_relationships();
    }
    if !pending && !new_dm {
        ui.horizontal(|ui| {
            ui.add(egui::TextEdit::singleline(&mut app.friend_input).hint_text("Nome de usuário"));
            if ui
                .add_enabled(
                    !app.friend_input.trim().is_empty(),
                    egui::Button::new("Adicionar amigo"),
                )
                .clicked()
            {
                app.request_friend(app.friend_input.trim().to_string());
            }
        });
    }
    if new_dm {
        ui.horizontal(|ui| {
            ui.add(egui::TextEdit::singleline(&mut app.friend_input).hint_text("ID do usuário"));
            if ui
                .add_enabled(
                    valid_user_id(app.friend_input.trim()),
                    egui::Button::new("Abrir conversa"),
                )
                .clicked()
            {
                app.open_dm(app.friend_input.trim().to_string());
            }
        });
        ui.label(
            RichText::new("Ou escolha um amigo abaixo.")
                .small()
                .color(theme::MUTED),
        );
    }
    if let Some(error) = &app.friend_error {
        ui.colored_label(theme::RED, error);
    }
    if app.relationships_loading {
        ui.spinner();
    }
    let relationships = app.relationships.clone();
    let mut count = 0;
    egui::ScrollArea::vertical().show(ui, |ui| {
        for relation in &relationships {
            if !visible_relationship(relation.kind, pending) {
                continue;
            }
            count += 1;
            ui.horizontal(|ui| {
                let texture = app.images.get(
                    ui.ctx(),
                    &app.handle,
                    &util::user_avatar_url(&relation.user),
                );
                let avatar = crate::ui::round_avatar(
                    ui,
                    texture.as_ref(),
                    36.0,
                    relation.user.display_name(),
                    util::name_color(relation.user.display_name()),
                );
                crate::ui::presence_dot(
                    ui,
                    avatar,
                    app.presence.get(&relation.user.id).map(String::as_str),
                    theme::CHAT,
                );
                ui.vertical(|ui| {
                    ui.label(RichText::new(relation.user.display_name()).strong());
                    ui.label(
                        RichText::new(if relation.kind == 3 {
                            "Solicitação recebida"
                        } else if relation.kind == 4 {
                            "Solicitação enviada"
                        } else {
                            &relation.user.username
                        })
                        .small()
                        .color(theme::MUTED),
                    );
                });
                if relation.kind == 1 && ui.button("Mensagem").clicked() {
                    app.open_dm(relation.user.id.clone());
                }
                if relation.kind == 3 && ui.button("Aceitar").clicked() {
                    app.accept_friend(relation.id.clone());
                }
                if ui
                    .button(if pending {
                        if relation.kind == 4 {
                            "Cancelar"
                        } else {
                            "Recusar"
                        }
                    } else {
                        "Remover amigo"
                    })
                    .clicked()
                {
                    app.remove_friend(relation.id.clone());
                }
            });
            ui.separator();
        }
    });
    if count == 0 && !app.relationships_loading {
        ui.label(if pending {
            "Nenhuma solicitação pendente."
        } else {
            "Nenhum amigo encontrado."
        });
    }
}

fn valid_user_id(id: &str) -> bool {
    !id.is_empty()
        && id.bytes().all(|c| c.is_ascii_digit())
        && id.parse::<u64>().is_ok_and(|id| id > 0)
}

fn visible_relationship(kind: u8, pending: bool) -> bool {
    if pending {
        matches!(kind, 3 | 4)
    } else {
        kind == 1
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn relationship_tabs_and_dm_ids() {
        assert!(visible_relationship(1, false));
        assert!(!visible_relationship(2, false));
        assert!(visible_relationship(3, true));
        assert!(visible_relationship(4, true));
        assert!(!visible_relationship(1, true));
        assert!(!visible_relationship(2, true));
        assert!(valid_user_id("172754870229532681"));
        for id in ["", "0", "-1", "name", "18446744073709551616"] {
            assert!(!valid_user_id(id));
        }
    }
}
