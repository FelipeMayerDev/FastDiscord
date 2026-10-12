//! Lazy member list: user accounts get a channel's member sidebar from the
//! gateway, not REST. Op 37 subscribes to row ranges of a channel's list and
//! Discord streams GUILD_MEMBER_LIST_UPDATE ops (SYNC, INSERT, UPDATE,
//! DELETE, INVALIDATE) over a flat list of group headers and members.

use std::collections::HashMap;

use serde::Deserialize;

use crate::model::User;

/// Discord sends rows in windows of 100, and its client keeps at most three
/// ranges subscribed per channel.
pub const RANGE_SIZE: usize = 100;
pub const MAX_RANGES: usize = 3;

#[derive(Clone, Debug, Deserialize)]
pub struct Role {
    pub id: String,
    #[serde(default)]
    pub name: String,
    /// `0xRRGGBB`; 0 = no color.
    #[serde(default)]
    pub color: u32,
    #[serde(default)]
    pub position: i64,
}

/// A row: `{"group": {...}}` or `{"member": {...}}`.
#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ListItem {
    Group(ListGroup),
    Member(ListMember),
}

/// A section header: a hoisted role id, `online` or `offline`. Header rows
/// in `items` carry only the id; counts come in the update's `groups`.
#[derive(Clone, Debug, Deserialize)]
pub struct ListGroup {
    pub id: String,
    #[serde(default)]
    pub count: u64,
}

#[derive(Clone, Debug, Deserialize)]
pub struct ListMember {
    pub user: User,
    #[serde(default)]
    pub nick: Option<String>,
    #[serde(default)]
    pub roles: Vec<String>,
    #[serde(default)]
    pub presence: Option<ListPresence>,
}

impl ListMember {
    pub fn display_name(&self) -> &str {
        self.nick.as_deref().unwrap_or(self.user.display_name())
    }
}

#[derive(Clone, Debug, Deserialize)]
pub struct ListPresence {
    #[serde(default)]
    pub status: String,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(tag = "op", rename_all = "UPPERCASE")]
pub enum ListOp {
    Sync {
        range: [usize; 2],
        #[serde(default)]
        items: Vec<ListItem>,
    },
    Insert {
        index: usize,
        item: ListItem,
    },
    Update {
        index: usize,
        item: ListItem,
    },
    Delete {
        index: usize,
    },
    Invalidate {
        range: [usize; 2],
    },
    #[serde(other)]
    Other,
}

/// GUILD_MEMBER_LIST_UPDATE. `id` names the list: `everyone`, or a hash of
/// the channel's view overwrites, shared by channels with the same audience.
#[derive(Clone, Debug, Deserialize)]
pub struct MemberListUpdate {
    pub guild_id: String,
    pub id: String,
    /// Every group's current count, resent with each update.
    #[serde(default)]
    pub groups: Vec<ListGroup>,
    #[serde(default)]
    pub ops: Vec<ListOp>,
}

impl MemberListUpdate {
    /// Members carried by this update, to refresh presences and names.
    pub fn members(&self) -> impl Iterator<Item = &ListMember> {
        self.ops
            .iter()
            .flat_map(|op| match op {
                ListOp::Sync { items, .. } => items.iter().collect(),
                ListOp::Insert { item, .. } | ListOp::Update { item, .. } => vec![item],
                _ => Vec::new(),
            })
            .filter_map(|item| match item {
                ListItem::Member(member) => Some(member),
                ListItem::Group(_) => None,
            })
    }
}

/// One guild's list, for the channel subscribed last. `None` rows were
/// invalidated (left the subscribed ranges) and are skipped.
#[derive(Clone, Debug, Default)]
pub struct MemberList {
    pub id: String,
    pub items: Vec<Option<ListItem>>,
}

impl MemberList {
    pub fn apply(&mut self, update: MemberListUpdate) {
        if update.id != self.id {
            // Another channel's audience: start over on its SYNC; stray ops
            // for a list we no longer show are ignored.
            if !update
                .ops
                .iter()
                .any(|op| matches!(op, ListOp::Sync { .. }))
            {
                return;
            }
            *self = MemberList {
                id: update.id,
                items: Vec::new(),
            };
        }
        for op in update.ops {
            match op {
                ListOp::Sync {
                    range: [start, end],
                    items,
                } => {
                    let count = items.len();
                    if self.items.len() < start + count {
                        self.items.resize(start + count, None);
                    }
                    for (offset, item) in items.into_iter().enumerate() {
                        self.items[start + offset] = Some(item);
                    }
                    // A short page means the list ends here.
                    if count < end.saturating_sub(start) + 1 {
                        self.items.truncate(start + count);
                    }
                }
                ListOp::Insert { index, item } => {
                    if index > self.items.len() {
                        self.items.resize(index, None);
                    }
                    self.items.insert(index, Some(item));
                }
                ListOp::Update { index, item } => {
                    if let Some(row) = self.items.get_mut(index) {
                        *row = Some(item);
                    }
                }
                ListOp::Delete { index } => {
                    if index < self.items.len() {
                        self.items.remove(index);
                    }
                }
                ListOp::Invalidate {
                    range: [start, end],
                } => {
                    for row in self.items.iter_mut().take(end + 1).skip(start) {
                        *row = None;
                    }
                }
                ListOp::Other => {}
            }
        }
        for row in self.items.iter_mut().flatten() {
            if let ListItem::Group(group) = row
                && let Some(fresh) = update.groups.iter().find(|g| g.id == group.id)
            {
                group.count = fresh.count;
            }
        }
    }
}

/// Member sidebar state: lists per guild, guild roles for the hoisted group
/// names and name colors, and the subscription last sent.
#[derive(Default)]
pub struct Members {
    pub lists: HashMap<String, MemberList>,
    pub roles: HashMap<String, HashMap<String, Role>>,
    /// Channel and how many ranges of it we subscribed to.
    pub subscribed: Option<(String, usize)>,
}

impl Members {
    /// The highest colored role's color, like Discord's name color.
    pub fn color(&self, guild_id: &str, member: &ListMember) -> Option<u32> {
        let roles = self.roles.get(guild_id)?;
        member
            .roles
            .iter()
            .filter_map(|id| roles.get(id))
            .filter(|role| role.color != 0)
            .max_by_key(|role| role.position)
            .map(|role| role.color)
    }
}

/// Op 37 for one channel's rows `0..ranges * 100`. Only `channels` is sent,
/// so the guild's typing/activities/threads subscription stays as it was.
pub fn subscribe_payload(guild_id: &str, channel_id: &str, ranges: usize) -> String {
    let ranges: Vec<[usize; 2]> = (0..ranges.clamp(1, MAX_RANGES))
        .map(|index| [index * RANGE_SIZE, index * RANGE_SIZE + RANGE_SIZE - 1])
        .collect();
    serde_json::json!({
        "op": 37,
        "d": { "subscriptions": { guild_id: { "channels": { channel_id: ranges } } } }
    })
    .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn member(id: &str) -> serde_json::Value {
        json!({ "member": { "user": { "id": id, "username": id }, "roles": [],
            "presence": { "status": "online" } } })
    }

    fn update(id: &str, ops: serde_json::Value) -> MemberListUpdate {
        serde_json::from_value(json!({ "guild_id": "g", "id": id, "ops": ops })).unwrap()
    }

    fn ids(list: &MemberList) -> Vec<String> {
        list.items
            .iter()
            .map(|item| match item {
                Some(ListItem::Group(group)) => format!("[{}]", group.id),
                Some(ListItem::Member(member)) => member.user.id.clone(),
                None => "-".into(),
            })
            .collect()
    }

    #[test]
    fn applies_sync_insert_update_delete_and_invalidate() {
        let mut list = MemberList::default();
        // Ops for a list never synced are ignored.
        list.apply(update("everyone", json!([{ "op": "DELETE", "index": 0 }])));
        assert!(list.items.is_empty());

        list.apply(update(
            "everyone",
            json!([{ "op": "SYNC", "range": [0, 99], "items": [
                { "group": { "id": "123", "count": 1 } }, member("a"),
                { "group": { "id": "online", "count": 2 } }, member("b"), member("c")
            ] }]),
        ));
        assert_eq!(ids(&list), ["[123]", "a", "[online]", "b", "c"]);

        list.apply(update(
            "everyone",
            json!([
                { "op": "INSERT", "index": 4, "item": member("d") },
                { "op": "UPDATE", "index": 1, "item": member("e") },
                { "op": "DELETE", "index": 3 },
                { "op": "CUSTOM_NEW_OP" }
            ]),
        ));
        assert_eq!(ids(&list), ["[123]", "e", "[online]", "d", "c"]);

        list.apply(update(
            "everyone",
            json!([{ "op": "INVALIDATE", "range": [3, 99] }]),
        ));
        assert_eq!(ids(&list), ["[123]", "e", "[online]", "-", "-"]);

        // A shorter SYNC trims the tail; another list id replaces the list.
        list.apply(update(
            "everyone",
            json!([{ "op": "SYNC", "range": [0, 99], "items": [member("a")] }]),
        ));
        assert_eq!(ids(&list), ["a"]);
        list.apply(update(
            "4127",
            json!([{ "op": "SYNC", "range": [0, 99], "items": [member("z")] }]),
        ));
        assert_eq!((list.id.as_str(), ids(&list)), ("4127", vec!["z".into()]));
    }

    #[test]
    fn header_counts_come_from_groups() {
        let counts = |list: &MemberList| -> Vec<(String, u64)> {
            list.items
                .iter()
                .flatten()
                .filter_map(|item| match item {
                    ListItem::Group(group) => Some((group.id.clone(), group.count)),
                    ListItem::Member(_) => None,
                })
                .collect()
        };
        let mut list = MemberList::default();
        // Header rows carry no count, as Discord sends them.
        list.apply(
            serde_json::from_value(json!({ "guild_id": "g", "id": "everyone",
                "groups": [{ "id": "online", "count": 1 }, { "id": "offline", "count": 1 }],
                "ops": [{ "op": "SYNC", "range": [0, 99], "items": [
                    { "group": { "id": "online" } }, member("a"),
                    { "group": { "id": "offline" } }, member("b")
                ] }] }))
            .unwrap(),
        );
        assert_eq!(counts(&list), [("online".into(), 1), ("offline".into(), 1)]);

        // b comes online: the ops move the row, `groups` carries the counts.
        list.apply(
            serde_json::from_value(json!({ "guild_id": "g", "id": "everyone",
                "groups": [{ "id": "online", "count": 2 }],
                "ops": [
                    { "op": "DELETE", "index": 3 },
                    { "op": "DELETE", "index": 2 },
                    { "op": "INSERT", "index": 2, "item": member("b") }
                ] }))
            .unwrap(),
        );
        assert_eq!(ids(&list), ["[online]", "a", "b"]);
        assert_eq!(counts(&list), [("online".into(), 2)]);
    }

    #[test]
    fn subscribes_up_to_three_ranges_and_colors_by_top_role() {
        let payload: serde_json::Value =
            serde_json::from_str(&subscribe_payload("g", "c", 5)).unwrap();
        assert_eq!(payload["op"], 37);
        assert_eq!(
            payload["d"]["subscriptions"]["g"]["channels"]["c"],
            json!([[0, 99], [100, 199], [200, 299]])
        );

        let mut members = Members::default();
        members.roles.insert(
            "g".into(),
            [("low", 0xff0000, 1), ("high", 0x00ff00, 5), ("plain", 0, 9)]
                .into_iter()
                .map(|(id, color, position)| {
                    let role = Role {
                        id: id.into(),
                        name: id.into(),
                        color,
                        position,
                    };
                    (id.to_string(), role)
                })
                .collect(),
        );
        let member: ListMember = serde_json::from_value(json!({
            "user": { "id": "1", "username": "u" }, "roles": ["low", "high", "plain"]
        }))
        .unwrap();
        assert_eq!(members.color("g", &member), Some(0x00ff00));
    }
}
