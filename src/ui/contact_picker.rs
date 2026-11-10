use std::{cell::RefCell, rc::Rc};

use crate::{AppState, Contact, ui::window::WindowParts};

pub(crate) fn display_name_for_jid(jid: &str) -> String {
	if jid == crate::types::META_AI_JID {
		return "Meta AI".to_string();
	}
	if jid == crate::types::WHATSAPP_SERVICE_JID {
		return "WhatsApp".to_string();
	}
	let raw_phone = jid.split('@').next().unwrap_or(jid);
	if !raw_phone.is_empty() && raw_phone.chars().all(|c| c.is_ascii_digit()) {
		if let Some(formatted) = whatsnap::phone::format_international(raw_phone) {
			return formatted;
		}
		return format!("+{}", raw_phone);
	}
	jid.to_string()
}

pub(crate) fn format_phone_digits(raw_phone: &str) -> String {
	if let Some(formatted) = whatsnap::phone::format_international(raw_phone) {
		return formatted;
	}
	format!("+{}", raw_phone)
}

pub(crate) fn contact_picker_label(contact: &Contact) -> String {
	if contact.jid == crate::types::WHATSAPP_SERVICE_JID && contact.name.trim().is_empty() {
		return "WhatsApp".to_string();
	}
	let is_lid = contact.jid.ends_with("@lid");
	let raw_phone = if !contact.phone.is_empty() {
		Some(contact.phone.as_str())
	} else if !is_lid {
		Some(contact.jid.split('@').next().unwrap_or(&contact.jid))
	} else {
		None
	};
	let formatted_phone =
		raw_phone.filter(|p| !p.is_empty() && p.chars().all(|c| c.is_ascii_digit())).map(format_phone_digits);
	match (contact.name.trim().is_empty(), formatted_phone) {
		(false, Some(phone)) => format!("{} {}", contact.name.trim(), phone),
		(false, None) => contact.name.trim().to_string(),
		(true, Some(phone)) => phone,
		(true, None) => contact.jid.clone(),
	}
}

/// Rebuilds `messages_select_list` from the current `display_items` while
/// selection mode is active - a no-op otherwise. Called everywhere
/// `messages_list` itself gets a structural rebuild (new message, delete,
/// history load) so the selection list never shows a stale row set while the
/// user is mid-selection. Previously-checked message IDs that no longer
/// exist in the rebuilt rows (e.g. deleted from another device) are dropped
/// rather than left dangling in `selected_message_ids`.
pub(crate) fn refresh_selection_list(s: &mut AppState, parts: &WindowParts) {
	if !s.selection_mode {
		return;
	}
	let list = &parts.conversations_parts.messages_select_list;
	let cursor_id = list
		.get_selection()
		.and_then(|i| s.selection_rows.get(i as usize))
		.or_else(|| {
			let main = parts.conversations_parts.messages_list.get_selection()?;
			s.display_items.get(main as usize)
		})
		.map(|item| item.id().to_string());
	let is_group = s.selected_chat.as_deref().map(|j| j.ends_with("@g.us")).unwrap_or(false);
	s.selection_rows = s
		.display_items
		.iter()
		.filter(|item| match item {
			whatsnap::DisplayItem::Single(m) => !m.is_date_divider && !m.is_unread_divider,
			_ => true,
		})
		.cloned()
		.collect();
	let valid_ids: std::collections::HashSet<String> = s.selection_rows.iter().map(|i| i.id().to_string()).collect();
	s.selected_message_ids.retain(|id| valid_ids.contains(id));

	let rows: Vec<String> = s.selection_rows.iter().map(|item| item.format_display(is_group)).collect();
	list.set_items(&rows.iter().map(String::as_str).collect::<Vec<_>>());
	for (idx, item) in s.selection_rows.iter().enumerate() {
		if s.selected_message_ids.contains(item.id()) {
			list.check(idx as u32, true);
		}
	}
	let cursor = cursor_id.and_then(|id| s.selection_rows.iter().position(|item| item.id() == id));
	if let Some(pos) = cursor.or_else(|| rows.len().checked_sub(1)) {
		list.set_selection(pos as u32, true);
	}
	crate::ui_commands::selection::update_bar(s, &parts.conversations_parts);
}

/// Re-filters `s.all_contacts` by `query` and repaints the New Group dialog's
/// `contacts_listbox` (a wxCheckListBox) with the matches, preserving which
/// jids are already checked. Shared by opening the dialog and by a fresh
/// `contacts` response landing while it's already open - without the latter,
/// the box stays empty for the entire first `get_contacts` round trip, and
/// tabbing focus into an empty wxCheckListBox is what trips the "bad
/// wxCheckListBox index" debug assert in NVDA's accessibility queries.
pub(crate) fn refresh_new_group_contacts_list(query: &str, state: &Rc<RefCell<AppState>>, parts: &Rc<WindowParts>) {
	let q = query.trim().to_lowercase();
	let mut s = state.borrow_mut();
	let mut all_labels = Vec::new();
	let mut all_jids = Vec::new();
	for contact in &s.all_contacts {
		all_labels.push(contact_picker_label(contact));
		all_jids.push(contact.jid.clone());
	}
	let mut filtered_labels = Vec::new();
	let mut filtered_jids = Vec::new();
	for (lbl, jid) in all_labels.iter().zip(all_jids.iter()) {
		if q.is_empty() || lbl.to_lowercase().contains(&q) {
			filtered_labels.push(lbl.clone());
			filtered_jids.push(jid.clone());
		}
	}
	parts.new_group_dialog.contacts_listbox.clear();
	for lbl in &filtered_labels {
		parts.new_group_dialog.contacts_listbox.append(lbl);
	}
	let gs = &mut s.group_state;
	gs.all_contact_labels = all_labels;
	gs.all_contact_jids = all_jids;
	gs.current_jids = filtered_jids;
	for (idx, jid) in gs.current_jids.iter().enumerate() {
		if gs.checked_jids.contains(jid) {
			parts.new_group_dialog.contacts_listbox.check(idx as u32, true);
		}
	}
	// See the comment on the equivalent reset in window.rs's search handler:
	// clear()+append() leaves the native listbox caret wherever it was
	// before the rebuild, which NVDA can query past the end of a shorter
	// list and trip wx's "bad wxCheckListBox index" assert.
	// wxListBox::SetSelection asserts/crashes if given index 0 on an empty
	// list (only wxNOT_FOUND, which wxdragon's u32 API can't express, is
	// valid there) - so there's nothing to reset when the filter has no
	// matches, only when it has at least one.
	if !gs.current_jids.is_empty() {
		parts.new_group_dialog.contacts_listbox.set_selection(0, true);
	}
}

pub(crate) fn sync_new_group_contact_labels(state: &Rc<RefCell<AppState>>) {
	let mut s = state.borrow_mut();
	let mut all_labels = Vec::new();
	let mut all_jids = Vec::new();
	for contact in &s.all_contacts {
		all_labels.push(contact_picker_label(contact));
		all_jids.push(contact.jid.clone());
	}
	let gs = &mut s.group_state;
	gs.all_contact_labels = all_labels;
	gs.all_contact_jids = all_jids;
}

pub(crate) fn refresh_add_members_contacts_list(query: &str, state: &Rc<RefCell<AppState>>, parts: &Rc<WindowParts>) {
	let q = query.trim().to_lowercase();
	let mut s = state.borrow_mut();
	let existing_members: std::collections::HashSet<String> = s.group_data_member_jids.iter().cloned().collect();
	let mut all_labels = Vec::new();
	let mut all_jids = Vec::new();
	for contact in &s.all_contacts {
		// WA Web's member picker leaves out bots, Meta AI included.
		if existing_members.contains(&contact.jid) || contact.jid.ends_with("@bot") {
			continue;
		}
		all_labels.push(contact_picker_label(contact));
		all_jids.push(contact.jid.clone());
	}
	let mut filtered_labels = Vec::new();
	let mut filtered_jids = Vec::new();
	for (lbl, jid) in all_labels.iter().zip(all_jids.iter()) {
		if q.is_empty() || lbl.to_lowercase().contains(&q) {
			filtered_labels.push(lbl.clone());
			filtered_jids.push(jid.clone());
		}
	}
	parts.add_members_dialog.contacts_listbox.clear();
	for lbl in &filtered_labels {
		parts.add_members_dialog.contacts_listbox.append(lbl);
	}
	let ams = &mut s.add_members_state;
	ams.all_contact_labels = all_labels;
	ams.all_contact_jids = all_jids;
	ams.current_jids = filtered_jids;
	for (idx, jid) in ams.current_jids.iter().enumerate() {
		if ams.checked_jids.contains(jid) {
			parts.add_members_dialog.contacts_listbox.check(idx as u32, true);
		}
	}
	// See the equivalent reset in refresh_new_group_contacts_list/window.rs:
	// clear()+append() leaves the native listbox caret wherever it was
	// before the rebuild, which NVDA can query past the end of a shorter
	// list and trip wx's "bad wxCheckListBox index" assert. SetSelection
	// itself asserts/crashes given index 0 on an empty list (only
	// wxNOT_FOUND, which wxdragon's u32 API can't express, is valid there).
	if !ams.current_jids.is_empty() {
		parts.add_members_dialog.contacts_listbox.set_selection(0, true);
	}
}

pub(crate) fn sync_add_members_contact_labels(state: &Rc<RefCell<AppState>>) {
	let mut s = state.borrow_mut();
	let existing_members: std::collections::HashSet<String> = s.group_data_member_jids.iter().cloned().collect();
	let mut all_labels = Vec::new();
	let mut all_jids = Vec::new();
	for contact in &s.all_contacts {
		if existing_members.contains(&contact.jid) {
			continue;
		}
		all_labels.push(contact_picker_label(contact));
		all_jids.push(contact.jid.clone());
	}
	let ams = &mut s.add_members_state;
	ams.all_contact_labels = all_labels;
	ams.all_contact_jids = all_jids;
}

pub(crate) fn refresh_attach_contact_list(query: &str, state: &Rc<RefCell<AppState>>, parts: &Rc<WindowParts>) {
	let q = query.trim().to_lowercase();
	let mut s = state.borrow_mut();
	let mut all_labels = Vec::new();
	let mut all_jids = Vec::new();
	for contact in &s.all_contacts {
		all_labels.push(contact_picker_label(contact));
		all_jids.push(contact.jid.clone());
	}
	let mut filtered_labels = Vec::new();
	let mut filtered_jids = Vec::new();
	for (lbl, jid) in all_labels.iter().zip(all_jids.iter()) {
		if q.is_empty() || lbl.to_lowercase().contains(&q) {
			filtered_labels.push(lbl.clone());
			filtered_jids.push(jid.clone());
		}
	}
	parts.attach_contact_dialog.contacts_listbox.clear();
	for lbl in &filtered_labels {
		parts.attach_contact_dialog.contacts_listbox.append(lbl);
	}
	let acs = &mut s.attach_contact_state;
	acs.all_contact_labels = all_labels;
	acs.all_contact_jids = all_jids;
	acs.current_jids = filtered_jids;
	for (idx, jid) in acs.current_jids.iter().enumerate() {
		if acs.checked_jids.contains(jid) {
			parts.attach_contact_dialog.contacts_listbox.check(idx as u32, true);
		}
	}
	if !acs.current_jids.is_empty() {
		parts.attach_contact_dialog.contacts_listbox.set_selection(0, true);
	}
}

pub(crate) fn sync_attach_contact_labels(state: &Rc<RefCell<AppState>>) {
	let mut s = state.borrow_mut();
	let mut all_labels = Vec::new();
	let mut all_jids = Vec::new();
	for contact in &s.all_contacts {
		all_labels.push(contact_picker_label(contact));
		all_jids.push(contact.jid.clone());
	}
	let acs = &mut s.attach_contact_state;
	acs.all_contact_labels = all_labels;
	acs.all_contact_jids = all_jids;
}

pub(crate) fn apply_contact_picker_filter(query: &str, state: &Rc<RefCell<AppState>>, parts: &Rc<WindowParts>) {
	let q = query.trim().to_lowercase();
	let q_digits: String = query.chars().filter(|c| c.is_ascii_digit()).collect();
	let mut s = state.borrow_mut();
	let mut labels = Vec::new();
	let mut jids = Vec::new();
	let own_jid_str = crate::ui::owner_draw::own_jid();
	let own_digits = own_jid_str.split('@').next().unwrap_or("");

	if q_digits.len() >= 7 {
		let resolved = whatsnap::phone::valid_digits(&q_digits)
			.or_else(|| whatsnap::phone::with_own_country(own_digits, &q_digits))
			.unwrap_or_else(|| q_digits.clone());
		let candidate_jid = format!("{resolved}@s.whatsapp.net");
		let is_already_saved = s.all_contacts.iter().any(|c| {
			let c_phone_digits: String = c.phone.chars().filter(|ch| ch.is_ascii_digit()).collect();
			let c_jid_digits: String =
				c.jid.split('@').next().unwrap_or("").chars().filter(|ch| ch.is_ascii_digit()).collect();
			(!c_phone_digits.is_empty() && c_phone_digits == resolved)
				|| (!c_jid_digits.is_empty() && c_jid_digits == resolved)
		});

		if !is_already_saved {
			let formatted = whatsnap::phone::format_international(&resolved).unwrap_or_else(|| format!("+{resolved}"));
			let action_label = match s.contact_picker_mode {
				crate::ContactPickerMode::OpenChat => format!("Chat with {formatted}"),
				crate::ContactPickerMode::AddCallParticipant => format!("Add {formatted} to call"),
				crate::ContactPickerMode::BlockContact => format!("Block {formatted}"),
			};
			labels.push(action_label);
			jids.push(candidate_jid);
		}
	}

	for contact in &s.all_contacts {
		let label = contact_picker_label(contact);
		let matches_name = q.is_empty() || label.to_lowercase().contains(&q);
		let matches_phone = if !q_digits.is_empty() {
			let c_phone_digits: String = contact.phone.chars().filter(|c| c.is_ascii_digit()).collect();
			let c_jid_digits: String =
				contact.jid.split('@').next().unwrap_or("").chars().filter(|c| c.is_ascii_digit()).collect();
			c_phone_digits.contains(&q_digits) || c_jid_digits.contains(&q_digits)
		} else {
			false
		};

		if matches_name || matches_phone {
			labels.push(label);
			jids.push(contact.jid.clone());
		}
	}

	parts.new_conv_dialog.results_list.clear();
	for label in &labels {
		parts.new_conv_dialog.results_list.append(label);
	}
	if !labels.is_empty() {
		parts.new_conv_dialog.results_list.set_selection(0, true);
	}
	s.contact_picker_current_jids = jids;

	let is_open_chat = s.contact_picker_mode == crate::ContactPickerMode::OpenChat;
	if is_open_chat {
		if q.is_empty() {
			parts.new_conv_dialog.contacts_label.set_label(&format!("Contacts on WhatsApp ({})", labels.len()));
		} else {
			parts.new_conv_dialog.contacts_label.set_label(&format!("Search results ({})", labels.len()));
		}
	}

	if !q.is_empty() {
		let msg = match labels.len() {
			0 => "No contacts found.".to_string(),
			1 => format!("1 contact found: {}", labels[0]),
			n => format!("{n} contacts found."),
		};
		crate::platform::announce::announce(parts.new_conv_dialog.announcer, &msg);
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn the_whatsapp_news_account_is_named_not_shown_as_a_number() {
		assert_eq!(display_name_for_jid(crate::types::WHATSAPP_SERVICE_JID), "WhatsApp");
		let unnamed = Contact {
			jid: crate::types::WHATSAPP_SERVICE_JID.to_string(),
			name: String::new(),
			phone: "0".to_string(),
		};
		assert_eq!(contact_picker_label(&unnamed), "WhatsApp");
	}
}
