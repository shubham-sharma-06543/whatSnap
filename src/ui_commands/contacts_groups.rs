use std::{cell::RefCell, rc::Rc};

use wxdragon::prelude::*;

use crate::{
	AppState, ContactPickerMode, MemberAction,
	platform::tray::refresh_tray_tooltip,
	ui::{
		chat_ui::{clear_pending_reply, open_chat_for_jid},
		contact_picker::{
			apply_contact_picker_filter, refresh_add_members_contacts_list, refresh_new_group_contacts_list,
		},
		message_display::apply_chat_filter,
		window::WindowParts,
	},
};

pub(crate) fn prepare_new_group(state: &Rc<RefCell<AppState>>, parts: &Rc<WindowParts>) {
	parts.new_group_dialog.ok_btn.enable(true);
	parts.new_group_dialog.name_field.set_value("");
	parts.new_group_dialog.search_field.set_value("");
	parts.new_group_dialog.extra_number_field.set_value("");
	state.borrow_mut().group_state.checked_jids.clear();
	refresh_new_group_contacts_list("", state, parts);
	let payload = serde_json::json!({ "action": "get_contacts" });
	let _ = state.borrow().ws_tx.send(payload.to_string());
	crate::ui::show_modal_again(&parts.new_group_dialog.dialog);
}

pub(crate) fn prepare_add_members(state: &Rc<RefCell<AppState>>, parts: &Rc<WindowParts>) {
	let group_jid = state.borrow().selected_chat.clone();
	if group_jid.is_none() {
		return;
	}
	parts.add_members_dialog.add_btn.enable(true);
	parts.add_members_dialog.search_field.set_value("");
	parts.add_members_dialog.selection_announcer.set_label("");
	{
		let mut s = state.borrow_mut();
		s.add_members_state.group_jid = group_jid;
		s.add_members_state.checked_jids.clear();
	}
	refresh_add_members_contacts_list("", state, parts);
	let payload = serde_json::json!({ "action": "get_contacts" });
	let _ = state.borrow().ws_tx.send(payload.to_string());
	crate::ui::show_modal_again(&parts.add_members_dialog.dialog);
}

pub(crate) fn add_group_members(state: &Rc<RefCell<AppState>>, _parts: &Rc<WindowParts>, participants: Vec<String>) {
	let group_jid = state.borrow().add_members_state.group_jid.clone();
	if let Some(group_jid) = group_jid
		&& !participants.is_empty()
	{
		let payload = serde_json::json!({
			"action": "update_group_participants",
			"data": { "jid": group_jid, "participants": participants, "action": "add" }
		});
		let _ = state.borrow().ws_tx.send(payload.to_string());
	}
}

pub(crate) fn create_group(
	state: &Rc<RefCell<AppState>>,
	_parts: &Rc<WindowParts>,
	name: String,
	participants: Vec<String>,
) {
	let payload = serde_json::json!({
		"action": "create_group",
		"data": {
			"name": name,
			"participants": participants
		}
	});
	let _ = state.borrow().ws_tx.send(payload.to_string());
}

pub(crate) fn new_contact(
	state: &Rc<RefCell<AppState>>,
	_parts: &Rc<WindowParts>,
	name: String,
	first_name: String,
	phone: String,
	sync: bool,
) {
	let payload = serde_json::json!({
		"action": "new_contact",
		"data": {
			"phone": phone,
			"name": name,
			"first_name": first_name,
			"sync": sync
		}
	});
	let _ = state.borrow().ws_tx.send(payload.to_string());
}

pub(crate) fn filter_group_members(state: &Rc<RefCell<AppState>>, parts: &Rc<WindowParts>, query: String) {
	let mut s = state.borrow_mut();
	let lower_query = query.to_lowercase();
	parts.group_data_dialog.members_list.clear();
	let mut visible_jids = Vec::new();
	for (i, member) in s.group_data_members.iter().enumerate() {
		if lower_query.is_empty() || member.to_lowercase().contains(&lower_query) {
			parts.group_data_dialog.members_list.append(member);
			visible_jids.push(s.group_data_member_jids[i].clone());
		}
	}
	s.group_data_visible_member_jids = visible_jids;
}

pub(crate) fn prepare_contact_picker(state: &Rc<RefCell<AppState>>, parts: &Rc<WindowParts>, mode: ContactPickerMode) {
	if mode == ContactPickerMode::AddCallParticipant && state.borrow().active_call_id.is_none() {
		crate::platform::announce::announce(parts.conversations_parts.messages_announcer, "No active call.");
		return;
	}
	state.borrow_mut().contact_picker_mode = mode;
	parts.new_conv_dialog.search_field.set_value("");
	let (win_title, heading, list_heading) = match mode {
		ContactPickerMode::OpenChat => ("New Chat", "Search name or number", "Contacts on WhatsApp"),
		ContactPickerMode::AddCallParticipant => (
			"Add to Call",
			"Search a contact to add to the call, then press Enter",
			"Select contact to add",
		),
		ContactPickerMode::BlockContact => {
			("Block Contact", "Search a contact to block, then press Enter", "Select contact to block")
		}
	};
	parts.new_conv_dialog.dialog.set_label(win_title);
	parts.new_conv_dialog.search_label.set_label(heading);
	parts.new_conv_dialog.contacts_label.set_label(list_heading);
	let is_open_chat = mode == ContactPickerMode::OpenChat;
	parts.new_conv_dialog.action_panel.show(is_open_chat);
	parts.new_conv_dialog.message_meta_ai_btn.show(is_open_chat && state.borrow().settings.meta_ai_enabled);
	parts.new_conv_dialog.dialog.layout();
	apply_contact_picker_filter("", state, parts);
	parts.new_conv_dialog.search_field.set_focus();
	let payload = serde_json::json!({ "action": "get_contacts" });
	let _ = state.borrow().ws_tx.send(payload.to_string());
	crate::ui::show_modal_again(&parts.new_conv_dialog.dialog);
}

pub(crate) fn filter_contact_picker(state: &Rc<RefCell<AppState>>, parts: &Rc<WindowParts>, query: String) {
	apply_contact_picker_filter(&query, state, parts);
}

pub(crate) fn select_contact_picker_result(state: &Rc<RefCell<AppState>>, parts: &Rc<WindowParts>, idx: usize) {
	let (jid_opt, mode) = {
		let s = state.borrow();
		(s.contact_picker_current_jids.get(idx).cloned(), s.contact_picker_mode)
	};
	let Some(jid) = jid_opt else {
		return;
	};
	parts.new_conv_dialog.dialog.end_modal(wxdragon::id::ID_OK);
	match mode {
		ContactPickerMode::OpenChat => {
			open_chat_for_jid(&jid, state, parts);
		}
		ContactPickerMode::AddCallParticipant => {
			let s = state.borrow();
			if let Some(call_id) = &s.active_call_id {
				let msg = serde_json::json!({
					"action": "add_participant",
					"data": { "call_id": call_id, "jid": jid }
				});
				let _ = s.ws_tx.send(msg.to_string());
				drop(s);
				crate::platform::announce::announce(parts.conversations_parts.messages_announcer, "Adding to call...");
			}
		}
		ContactPickerMode::BlockContact => crate::ui_commands::blocking::block_picked_contact(state, parts, jid),
	}
}

pub(crate) fn update_group_settings(
	state: &Rc<RefCell<AppState>>,
	parts: &Rc<WindowParts>,
	name: String,
	topic: String,
	announce: bool,
	locked: bool,
) {
	let jid = state.borrow().selected_chat.clone();
	if let Some(jid) = jid {
		let s = state.borrow();

		let msg_name = serde_json::json!({
			"action": "set_group_name",
			"data": { "jid": jid, "name": name }
		});
		let _ = s.ws_tx.send(msg_name.to_string());

		let msg_topic = serde_json::json!({
			"action": "set_group_topic",
			"data": { "jid": jid, "topic": topic }
		});
		let _ = s.ws_tx.send(msg_topic.to_string());

		let msg_ann = serde_json::json!({
			"action": "set_group_announce",
			"data": { "jid": jid, "value": announce }
		});
		let _ = s.ws_tx.send(msg_ann.to_string());

		let msg_lock = serde_json::json!({
			"action": "set_group_locked",
			"data": { "jid": jid, "value": locked }
		});
		let _ = s.ws_tx.send(msg_lock.to_string());

		crate::platform::announce::announce(parts.conversations_parts.messages_announcer, "Group settings updated.");
	}
}

/// Runs one item of a member's context menu in Group Info. Make admin and
/// Remove ask first, like WA Web; Dismiss as admin doesn't.
pub(crate) fn group_member_action(
	state: &Rc<RefCell<AppState>>,
	parts: &Rc<WindowParts>,
	member_jid: String,
	action: MemberAction,
) {
	let s = state.borrow();
	let Some(group_jid) = s.selected_chat.clone().filter(|j| j.ends_with("@g.us")) else { return };
	let Some(member) = s.group_data_member_info.iter().find(|m| m.jid == member_jid).cloned() else { return };
	let subject =
		s.contacts.get(&group_jid).cloned().unwrap_or_else(|| parts.group_data_dialog.group_name_field.get_value());
	drop(s);
	let dialog = &parts.group_data_dialog.dialog;
	let ask = |text: &str, title: &str, yes: &str| {
		let confirm = MessageDialog::builder(dialog, text, title)
			.with_style(MessageDialogStyle::YesNo | MessageDialogStyle::IconQuestion)
			.build();
		confirm.set_yes_no_labels(yes, "Cancel");
		confirm.show_modal() == wxdragon::id::ID_YES
	};
	let change = |what: &str| {
		let msg = serde_json::json!({
			"action": "update_group_participants",
			"data": { "jid": group_jid, "participants": [member.jid], "action": what }
		});
		let _ = state.borrow().ws_tx.send(msg.to_string());
	};
	match action {
		MemberAction::Message => {
			dialog.end_modal(wxdragon::id::ID_OK);
			open_chat_for_jid(member.chat_jid(), state, parts);
		}
		MemberAction::Info => {
			crate::ui_commands::chat_list::show_contact_info(state, parts, member.chat_jid().to_string());
		}
		MemberAction::AddContact => {
			if member.phone_jid.is_empty() {
				MessageDialog::builder(
					dialog,
					"WhatsApp hasn't shared this person's phone number, so they can't be saved as a contact.",
					"Add Contact",
				)
				.with_style(MessageDialogStyle::OK | MessageDialogStyle::IconInformation)
				.build()
				.show_modal();
				return;
			}
			let number = member.phone_jid.split('@').next().unwrap_or_default();
			let name = if member.name == member.phone { "" } else { member.name.as_str() };
			open_contact_form(parts, "Add Contact", "Save", name, "", &format!("+{}", number), false);
		}
		MemberAction::MakeAdmin => {
			if ask(
				&format!("Make {} an admin for \"{}\" group?", member.name, subject),
				"Make Group Admin",
				"Make group admin",
			) {
				change("promote");
			}
		}
		MemberAction::DismissAdmin => change("demote"),
		MemberAction::VerifyCode => crate::ui_commands::member_extras::request_security_code(state, parts, &member),
		MemberAction::SendHistory => {
			let count = member.history_count;
			let what = if count == 1 { "1 recent message".to_string() } else { format!("{count} recent messages") };
			if ask(
				&format!(
					"Send message history to {}?\n\n{} will get {} from this group.",
					member.name, member.name, what
				),
				"Send Message History",
				"Send",
			) {
				crate::ui_commands::member_extras::send_history(state, parts, &group_jid, &member);
			}
		}
		MemberAction::Remove => {
			if ask(&format!("Remove {} from \"{}\" group?", member.name, subject), "Remove Member", "Remove") {
				change("remove");
			}
		}
	}
}

pub(crate) fn join_community_subgroup(state: &Rc<RefCell<AppState>>, parts: &Rc<WindowParts>, idx: u32) {
	let s = state.borrow();
	let Some(subgroup_jid) = s.group_data_subgroup_jids.get(idx as usize).cloned() else {
		return;
	};
	if s.group_data_subgroup_joined.get(idx as usize).copied().unwrap_or(false) {
		drop(s);
		parts.group_data_dialog.dialog.end_modal(wxdragon::id::ID_OK);
		open_chat_for_jid(&subgroup_jid, state, parts);
		return;
	}
	let msg = serde_json::json!({
		"action": "join_community_subgroup",
		"data": { "jid": subgroup_jid, "community": s.group_data_community.clone().unwrap_or_default() }
	});
	let _ = s.ws_tx.send(msg.to_string());
	drop(s);
	crate::platform::announce::announce(parts.conversations_parts.messages_announcer, "Joining group...");
}

pub(crate) fn community_subgroup_selection_changed(state: &Rc<RefCell<AppState>>, parts: &Rc<WindowParts>) {
	let joined = parts
		.group_data_dialog
		.groups_list
		.get_selection()
		.and_then(|idx| state.borrow().group_data_subgroup_joined.get(idx as usize).copied())
		.unwrap_or(false);
	parts.group_data_dialog.join_group_btn.set_label(if joined { "Open group" } else { "Join group" });
}

pub(crate) fn open_group_invite(state: &Rc<RefCell<AppState>>, _parts: &Rc<WindowParts>, message_id: String) {
	let msg = serde_json::json!({
		"action": "get_group_invite_info",
		"data": { "message_id": message_id }
	});
	let _ = state.borrow().ws_tx.send(msg.to_string());
}

pub(crate) fn join_group_invite(state: &Rc<RefCell<AppState>>, _parts: &Rc<WindowParts>, message_id: String) {
	let msg = serde_json::json!({
		"action": "join_group_invite",
		"data": { "message_id": message_id }
	});
	let _ = state.borrow().ws_tx.send(msg.to_string());
}

pub(crate) fn leave_group(state: &Rc<RefCell<AppState>>, parts: &Rc<WindowParts>, jid: String) {
	let chat_name = {
		let mut s = state.borrow_mut();
		s.chats.retain(|j| j != &jid);
		s.filtered_chats.retain(|j| j != &jid);
		s.archived_jids.remove(&jid);
		s.pinned_jids.remove(&jid);
		s.muted_jids.remove(&jid);
		s.unread_counts.remove(&jid);
		s.draft_messages.remove(&jid);
		let name = s.contacts.get(&jid).cloned().unwrap_or_else(|| jid.clone());
		if s.selected_chat.as_ref() == Some(&jid) {
			s.selected_chat = None;
			clear_pending_reply(&mut s, parts);
			parts.conversations_parts.conversation_panel.show(false);
		}
		apply_chat_filter(&mut s, parts);
		refresh_tray_tooltip(&s, parts);
		name
	};
	let payload = serde_json::json!({
		"action": "leave_group",
		"data": {
			"jid": jid
		}
	});
	let _ = state.borrow().ws_tx.send(payload.to_string());
	crate::platform::announce::announce(
		parts.conversations_parts.messages_announcer,
		&format!("Left group {}.", chat_name),
	);
}

pub(crate) fn set_member_label(state: &Rc<RefCell<AppState>>, parts: &Rc<WindowParts>, label: String) {
	let s = state.borrow();
	let Some(jid) = s.selected_chat.clone().filter(|j| j.ends_with("@g.us")) else {
		return;
	};
	let payload = serde_json::json!({
		"action": "set_member_label",
		"data": { "jid": jid, "label": label }
	});
	let _ = s.ws_tx.send(payload.to_string());
	let text = if label.is_empty() { "Removing your tag..." } else { "Setting your tag..." };
	crate::platform::announce::announce(parts.conversations_parts.messages_announcer, text);
}

#[derive(Clone, Default)]
pub(crate) struct ContactCard {
	pub jid: String,
	pub saved: bool,
	pub first_name: String,
	pub full_name: String,
	pub push_name: String,
	pub phone: String,
}

impl ContactCard {
	pub(crate) fn from_user_info(data: &serde_json::Value) -> Self {
		let text = |key: &str| data.get(key).and_then(|v| v.as_str()).unwrap_or_default().to_string();
		ContactCard {
			jid: text("jid"),
			saved: data.get("saved").and_then(|v| v.as_bool()).unwrap_or(false),
			first_name: text("firstName"),
			full_name: text("fullName"),
			push_name: text("pushName"),
			phone: text("phone"),
		}
	}

	pub(crate) fn form_names(&self) -> (String, String) {
		if !self.saved {
			return (self.push_name.trim().to_string(), String::new());
		}
		let first = if self.first_name.is_empty() { &self.full_name } else { &self.first_name };
		let surname = self.full_name.strip_prefix(first.as_str()).unwrap_or_default().trim();
		(first.trim().to_string(), surname.to_string())
	}
}

pub(crate) fn show_contact_card(parts: &Rc<WindowParts>, card: Option<&ContactCard>) {
	let info = &parts.conversation_data_dialog;
	let saved = card.is_some_and(|c| c.saved);
	info.contact_btn.set_label(if saved { "Edit contact" } else { "Add contact" });
	info.contact_btn.enable(card.is_some());
	info.delete_contact_btn.show(saved);
	info.info_panel.layout();
}

pub(crate) fn open_new_contact_form(parts: &Rc<WindowParts>) {
	open_contact_form(parts, "New Contact", "Create", "", "", "", true);
}

fn open_contact_form(
	parts: &Rc<WindowParts>,
	title: &str,
	ok_label: &str,
	name: &str,
	surname: &str,
	phone: &str,
	phone_editable: bool,
) {
	let form = &parts.new_contact_dialog;
	form.dialog.set_label(title);
	form.ok_btn.set_label(ok_label);
	form.name_field.set_value(name);
	form.surname_field.set_value(surname);
	form.phone_field.set_value(phone);
	form.phone_field.set_editable(phone_editable);
	form.name_field.set_focus();
	crate::ui::show_modal_again(&form.dialog);
}

pub(crate) fn add_or_edit_contact(state: &Rc<RefCell<AppState>>, parts: &Rc<WindowParts>) {
	let Some(card) = state.borrow().conversation_data_contact.clone() else { return };
	if card.phone.is_empty() {
		MessageDialog::builder(
			&parts.conversation_data_dialog.details_dialog,
			"WhatsApp hasn't shared this person's phone number, so they can't be saved as a contact.",
			"Add Contact",
		)
		.with_style(MessageDialogStyle::OK | MessageDialogStyle::IconInformation)
		.build()
		.show_modal();
		return;
	}
	let (name, surname) = card.form_names();
	let phone = format!("+{}", card.phone);
	let title = if card.saved { "Edit Contact" } else { "Add Contact" };
	open_contact_form(parts, title, "Save", &name, &surname, &phone, false);
}

pub(crate) fn delete_contact(state: &Rc<RefCell<AppState>>, parts: &Rc<WindowParts>) {
	let Some(card) = state.borrow().conversation_data_contact.clone().filter(|c| c.saved) else { return };
	let name = if card.full_name.is_empty() { &card.first_name } else { &card.full_name };
	let confirm = MessageDialog::builder(
		&parts.conversation_data_dialog.details_dialog,
		&format!("Delete {} from your contacts? They'll be removed from your phone's contacts too.", name),
		"Delete Contact",
	)
	.with_style(MessageDialogStyle::YesNo | MessageDialogStyle::IconQuestion)
	.build();
	confirm.set_yes_no_labels("Delete", "Cancel");
	if confirm.show_modal() != wxdragon::id::ID_YES {
		return;
	}
	let payload = serde_json::json!({ "action": "delete_contact", "data": { "jid": card.jid } });
	let _ = state.borrow().ws_tx.send(payload.to_string());
	crate::platform::announce::announce(parts.conversations_parts.messages_announcer, "Deleting contact...");
}

#[cfg(test)]
mod tests {
	use super::ContactCard;

	#[test]
	fn form_names_split_a_saved_name_and_start_a_new_one_from_the_whatsapp_name() {
		let saved = ContactCard {
			saved: true,
			first_name: "Alex".into(),
			full_name: "Alex Doe".into(),
			push_name: "Al".into(),
			..Default::default()
		};
		assert_eq!(saved.form_names(), ("Alex".into(), "Doe".into()));
		let only_full = ContactCard { saved: true, full_name: "Alex Doe".into(), ..Default::default() };
		assert_eq!(only_full.form_names(), ("Alex Doe".into(), String::new()));
		let unsaved = ContactCard { push_name: " Al ".into(), ..Default::default() };
		assert_eq!(unsaved.form_names(), ("Al".into(), String::new()));
	}
}
