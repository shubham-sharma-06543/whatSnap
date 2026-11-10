use std::rc::Rc;

use wxdragon::{
	event::{EventType, WxEvtHandler},
	prelude::*,
};

use crate::{
	UiCommand,
	keymap::{default_menu_label, message_list, voice},
	ui::{keys, message_list::CursorList, theme, wake::UiCommandSender},
};

fn enter_key_held() -> bool {
	use windows::Win32::UI::Input::KeyboardAndMouse::{GetAsyncKeyState, VK_RETURN};
	let state = unsafe { GetAsyncKeyState(i32::from(VK_RETURN.0)) };
	state as u16 & 0x8000 != 0
}

/// "1 message selected", spoken and shown as the selection changes.
pub(crate) fn selection_text(count: usize) -> String {
	format!("{} {} selected", count, if count == 1 { "message" } else { "messages" })
}

pub(crate) fn sanitize_export_filename(name: &str) -> String {
	let cleaned: String = name.chars().map(|c| if r#"\/:*?"<>|"#.contains(c) { '_' } else { c }).collect();
	let trimmed = cleaned.trim();
	if trimmed.is_empty() { "chat".to_string() } else { trimmed.to_string() }
}

/// The messages of a message list row, for its Delete: every photo of an
/// album, as WhatsApp Web deletes the whole album.
fn deletable_ids(s: &crate::AppState, row: u32) -> Vec<String> {
	s.display_items
		.get(row as usize)
		.map(|item| item.messages().iter().filter(|m| !m.is_date_divider).map(|m| m.id.clone()).collect())
		.unwrap_or_default()
}

fn set_dialog_owner(dialog: &impl WxWidget, owner: &impl WxWidget) {
	let dialog_handle = dialog.get_handle();
	let owner_handle = owner.get_handle();
	if dialog_handle.is_null() || owner_handle.is_null() {
		return;
	}
	unsafe {
		windows::Win32::UI::WindowsAndMessaging::SetWindowLongPtrW(
			windows::Win32::Foundation::HWND(dialog_handle),
			windows::Win32::UI::WindowsAndMessaging::GWLP_HWNDPARENT,
			owner_handle as isize,
		);
	}
}

fn wire_media_list(
	list: &ListBox,
	dialog: &Dialog,
	ui_tx: &UiCommandSender,
	state: &std::rc::Rc<std::cell::RefCell<crate::AppState>>,
	get_message: impl Fn(&crate::AppState, usize) -> Option<whatsnap::Msg> + Clone + 'static,
	with_media_actions: bool,
) {
	let list_enter = *list;
	let dialog_enter = *dialog;
	let tx_enter = ui_tx.clone();
	let state_enter = state.clone();
	let get_message_enter = get_message.clone();
	keys::on_enter(list, Rc::default(), move || {
		if let Some(idx) = list_enter.get_selection()
			&& let Some(msg) = get_message_enter(&state_enter.borrow(), idx as usize)
		{
			dialog_enter.end_modal(0);
			let other_chat = {
				let s = state_enter.borrow();
				s.info_chat.clone().filter(|jid| s.selected_chat.as_ref() != Some(jid))
			};
			let _ = tx_enter.send(match other_chat {
				Some(jid) => UiCommand::OpenChatAtMessage { jid, message_id: msg.id.clone() },
				None => UiCommand::JumpToSearchResult { message_id: msg.id.clone(), message_text: String::new() },
			});
		}
	});

	if with_media_actions {
		let list_ctx = *list;
		let tx_ctx = ui_tx.clone();
		let state_ctx = state.clone();
		list.bind_internal(EventType::CONTEXT_MENU, move |_event| {
			let Some(idx) = list_ctx.get_selection() else { return };
			let Some(msg) = get_message(&state_ctx.borrow(), idx as usize) else { return };

			let download_id = 4101;
			let open_id = 4102;
			let save_id = 4103;
			let is_downloaded = msg.is_media_downloaded();
			let mut menu_builder = Menu::builder();
			if is_downloaded {
				menu_builder = menu_builder.append_item(open_id, "&Open", "Open this file").append_item(
					save_id,
					"&Save as...",
					"Save this file to a chosen location",
				);
			} else {
				menu_builder = menu_builder.append_item(download_id, "&Download", "Download this file");
			}
			let mut popup_menu = menu_builder.build();

			let tx_menu = tx_ctx.clone();
			let message_id = msg.id.clone();
			popup_menu.on_selected(move |menu_evt| {
				let id = menu_evt.get_id();
				let cmd = if id == download_id {
					Some(UiCommand::DownloadMediaMessage(message_id.clone()))
				} else if id == open_id {
					Some(UiCommand::OpenMediaMessage(message_id.clone()))
				} else if id == save_id {
					Some(UiCommand::SaveMediaMessageAs(message_id.clone()))
				} else {
					None
				};
				if let Some(cmd) = cmd {
					let _ = tx_menu.send(cmd);
				}
			});

			list_ctx.popup_menu(&mut popup_menu, None);
			popup_menu.destroy_menu();
		});
	}
}

/// A group member's context menu in Group Info, with the items WA Web's
/// participant menu has, each only when it applies: Message and Contact
/// info for anyone but you, Add to contacts for someone not saved, and for
/// group admins Make group admin, Remove and Dismiss as admin. Your own row
/// gets Edit member tag. It opens on the Applications key, Shift+F10, a
/// right click, Enter or a double click, like a click on a member in WA Web.
fn wire_group_member_menu(
	parts: &WindowParts,
	ui_tx: &UiCommandSender,
	state: &std::rc::Rc<std::cell::RefCell<crate::AppState>>,
) {
	use crate::MemberAction;
	let list = parts.group_data_dialog.members_list;
	let tag_field = parts.group_data_dialog.my_tag_field;
	let tx = ui_tx.clone();
	let state = state.clone();
	let show = Rc::new(move || {
		let Some(idx) = list.get_selection() else { return };
		let (member, viewer_admin) = {
			let s = state.borrow();
			let Some(jid) = s.group_data_visible_member_jids.get(idx as usize) else { return };
			let Some(member) = s.group_data_member_info.iter().find(|m| &m.jid == jid).cloned() else { return };
			let group = s.selected_chat.clone().unwrap_or_default();
			(member, s.admin_group_jids.contains(&group))
		};
		let is_bot = member.jid.ends_with("@bot");
		let mut items: Vec<(i32, String, &str, Option<MemberAction>)> = Vec::new();
		if !member.me {
			items.push((
				4201,
				format!("&Message {}", member.name),
				"Open your chat with this member",
				Some(MemberAction::Message),
			));
			if !is_bot {
				items.push((
					4202,
					"&Contact info".to_string(),
					"See this member's info, call them, or save them as a contact",
					Some(MemberAction::Info),
				));
			}
			if !member.saved && !is_bot && !member.phone_jid.is_empty() {
				items.push((
					4203,
					"&Add to contacts".to_string(),
					"Save this member as a contact",
					Some(MemberAction::AddContact),
				));
			}
		}
		if viewer_admin && !member.me {
			if !member.admin && !is_bot {
				items.push((
					4204,
					"Make group &admin".to_string(),
					"Let this member change group info and settings and manage members",
					Some(MemberAction::MakeAdmin),
				));
			}
			if !member.super_admin {
				items.push((
					4205,
					"&Remove".to_string(),
					"Remove this member from the group",
					Some(MemberAction::Remove),
				));
			}
			if member.admin && !member.super_admin && !is_bot {
				items.push((
					4206,
					"&Dismiss as admin".to_string(),
					"Make this admin an ordinary member again",
					Some(MemberAction::DismissAdmin),
				));
			}
		}
		if !member.me && !is_bot {
			items.push((
				4208,
				"&Verify security code".to_string(),
				"Compare the security code with this member to check your chat is end-to-end encrypted",
				Some(MemberAction::VerifyCode),
			));
		}
		if member.history_count > 0 {
			items.push((
				4209,
				"Send message &history".to_string(),
				"Send this new member recent messages from before they joined",
				Some(MemberAction::SendHistory),
			));
		}
		if member.me {
			items.push((
				4207,
				"Edit member &tag".to_string(),
				"Change the tag shown next to your name in this group",
				None,
			));
		}
		if items.is_empty() {
			return;
		}
		let mut builder = Menu::builder();
		for (id, label, help, _) in &items {
			builder = builder.append_item(*id, label, help);
		}
		let mut menu = builder.build();
		let tx_menu = tx.clone();
		let jid = member.jid.clone();
		menu.on_selected(move |event| {
			let id = event.get_id();
			let Some((_, _, _, action)) = items.iter().find(|(item, ..)| *item == id) else { return };
			match action {
				Some(action) => {
					let _ = tx_menu.send(UiCommand::GroupMemberAction { jid: jid.clone(), action: *action });
				}
				None => tag_field.set_focus(),
			}
		});
		list.popup_menu(&mut menu, member_menu_position(&list, idx));
		menu.destroy_menu();
	});
	let show_ctx = show.clone();
	list.bind_internal(EventType::CONTEXT_MENU, move |_| show_ctx());
	let show_dbl = show.clone();
	list.on_item_double_clicked(move |_| show_dbl());
	// Key down only: the Enter that picks a menu item is released on the
	// list again, and must not open the menu a second time.
	list.on_key_down(move |event| {
		if let WindowEventData::Keyboard(ref key) = event
			&& keys::is_enter(key.get_key_code())
		{
			show();
			event.skip(false);
			return;
		}
		event.skip(true);
	});
}

/// Where a member's menu opens: just under the selected row, so a menu
/// opened from the keyboard isn't left wherever the mouse happens to be.
fn member_menu_position(list: &ListBox, idx: u32) -> Option<Point> {
	#[cfg(windows)]
	{
		use windows::Win32::{
			Foundation::{HWND, LPARAM, RECT, WPARAM},
			UI::WindowsAndMessaging::{LB_GETITEMRECT, SendMessageW},
		};
		let mut rect = RECT::default();
		let got = unsafe {
			SendMessageW(
				HWND(list.get_handle()),
				LB_GETITEMRECT,
				Some(WPARAM(idx as usize)),
				Some(LPARAM(&mut rect as *mut RECT as isize)),
			)
		};
		if got.0 != -1 && rect.bottom > 0 {
			return Some(Point::new(rect.left + 24, rect.bottom));
		}
	}
	#[cfg(not(windows))]
	let _ = (list, idx);
	None
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum LoginView {
	Phone,
	Qr,
	Pairing,
}

#[derive(Clone, Copy)]
pub struct LoginControls {
	pub login_panel: Dialog,
	pub phone_panel: Panel,
	pub qr_panel: Panel,
	pub pairing_panel: Panel,
	pub phone_field: TextCtrl,
	pub qrcode_cancel_btn: Button,
	pub pairing_code_display: TextCtrl,
}

#[allow(dead_code)]
pub struct WindowParts {
	pub frame: Frame,
	pub nav_list: ListBox,
	pub content_panel: Panel,
	pub login_panel: Dialog,
	pub main_panel: Panel,
	pub nav_panel: Panel,
	pub qrcode_dialog: Dialog,
	pub qrcode_instructions: StaticText,
	pub qrcode_display: StaticBitmap,
	pub qrcode_announcer: StaticText,
	pub qrcode_reload_btn: Button,
	pub qrcode_cancel_btn: Button,
	pub qr_panel: Panel,
	pub phone_panel: Panel,
	pub country_combo: ComboBox,
	pub phone_field: TextCtrl,
	pub pair_btn: Button,
	pub switch_to_qr_btn: Button,
	pub pairing_dialog: Dialog,
	pub pairing_code_display: TextCtrl,
	pub pairing_announcer: StaticText,
	pub pairing_cancel_btn: Button,
	pub pairing_panel: Panel,
	pub new_conv_dialog: crate::ui::dialogs::new_conversation::NewConversationDialogParts,
	pub new_contact_dialog: crate::ui::dialogs::new_contact::NewContactDialogParts,
	pub poll_dialog: crate::ui::dialogs::poll_dialog::PollDialogParts,
	pub new_group_dialog: crate::ui::dialogs::new_group::NewGroupDialogParts,
	pub add_members_dialog: crate::ui::dialogs::add_members::AddMembersDialogParts,
	pub attach_contact_dialog: crate::ui::dialogs::attach_contact::AttachContactDialogParts,
	pub new_group_contacts_timer: Rc<Timer<Dialog>>,
	pub add_members_contacts_timer: Rc<Timer<Dialog>>,
	pub attach_contact_timer: Rc<Timer<Dialog>>,
	pub attachment_dialog: crate::ui::dialogs::attachment_dialog::AttachmentDialogParts,
	pub call_parts: crate::ui::call_window::CallWindowParts,
	pub conversation_data_dialog: crate::ui::conversation_data::ConversationDataDialogParts,
	pub group_data_dialog: crate::ui::group_data::GroupDataDialogParts,
	pub conversations_parts: crate::ui::conversations::ConversationsPanelParts,
	pub settings_dialog: crate::ui::dialogs::settings_dialog::SettingsDialogParts,
	pub tray_icon: Rc<TaskBarIcon>,
}

impl LoginControls {
	pub fn from_parts(parts: &WindowParts) -> Self {
		Self {
			login_panel: parts.login_panel,
			phone_panel: parts.phone_panel,
			qr_panel: parts.qr_panel,
			pairing_panel: parts.pairing_panel,
			phone_field: parts.phone_field,
			qrcode_cancel_btn: parts.qrcode_cancel_btn,
			pairing_code_display: parts.pairing_code_display,
		}
	}

	pub fn switch_view(&self, view: LoginView) {
		match view {
			LoginView::Phone => {
				self.qr_panel.show(false);
				self.pairing_panel.show(false);
				self.phone_panel.show(true);
				self.login_panel.set_label("WhatsApp Login");
				self.login_panel.set_size(Size::new(500, 360));
				self.login_panel.layout();
				self.phone_field.set_focus();
			}
			LoginView::Qr => {
				self.phone_panel.show(false);
				self.pairing_panel.show(false);
				self.qr_panel.show(true);
				self.login_panel.set_label("WhatsApp Login - QR Code");
				self.login_panel.set_size(Size::new(550, 560));
				self.login_panel.layout();
				self.qrcode_cancel_btn.set_focus();
			}
			LoginView::Pairing => {
				self.phone_panel.show(false);
				self.qr_panel.show(false);
				self.pairing_panel.show(true);
				self.login_panel.set_label("WhatsApp Login - Pairing Code");
				self.login_panel.set_size(Size::new(480, 240));
				self.login_panel.layout();
				self.pairing_code_display.select_all();
				self.pairing_code_display.set_focus();
			}
		}
		self.login_panel.centre();
	}
}

pub fn switch_login_view(parts: &WindowParts, view: LoginView) {
	LoginControls::from_parts(parts).switch_view(view);
}

pub const MENU_ID_SETTINGS: i32 = 6001;
pub const MENU_ID_LOGOUT: i32 = 6002;
pub const MENU_ID_ACCOUNTS: i32 = 6063;
pub const MENU_ID_CHECK_UPDATES: i32 = 6061;
pub const MENU_ID_RECONNECT: i32 = 6003;
pub const MENU_ID_NAV_CONVERSATIONS: i32 = 6010;
pub const MENU_ID_NAV_STATUS: i32 = 6011;
pub const MENU_ID_NAV_COMMUNITIES: i32 = 6012;
pub const MENU_ID_NAV_STARRED: i32 = 6013;
pub const MENU_ID_NAV_ARCHIVED: i32 = 6014;
pub const MENU_ID_NAV_CHANNELS: i32 = 6015;
pub const MENU_ID_NAV_CALLS: i32 = 6016;
pub const MENU_ID_NEW_CHAT: i32 = 6020;
pub const MENU_ID_NEW_GROUP: i32 = 6021;
pub const MENU_ID_SEARCH_GLOBAL: i32 = 6022;
pub const MENU_ID_SEARCH_CHAT: i32 = 6023;
pub const MENU_ID_MUTE_CHAT: i32 = 6024;
pub const MENU_ID_ARCHIVE_CHAT: i32 = 6025;
pub const MENU_ID_PIN_CHAT: i32 = 6026;
pub const MENU_ID_MARK_UNREAD: i32 = 6027;
pub const MENU_ID_CHAT_INFO: i32 = 6028;
pub const MENU_ID_STAR_MESSAGE: i32 = 6029;
pub const MENU_ID_CALL_MUTE: i32 = 6033;
pub const MENU_ID_CALL_END: i32 = 6034;
pub const MENU_ID_LOCK_CHAT: i32 = 6035;
pub const MENU_ID_DISAPPEARING: i32 = 6036;
pub const MENU_ID_REPORT_CHAT: i32 = 6037;
pub const MENU_ID_BLOCK_CHAT: i32 = 6038;
pub const MENU_ID_BLOCKED_CONTACTS: i32 = 6039;
pub const MENU_ID_PINNED_MESSAGES: i32 = 6040;
pub const MENU_ID_NEW_COMMUNITY: i32 = 6042;
pub const MENU_ID_MANAGE_LISTS: i32 = 6043;
pub const MENU_ID_CREATE_CALL_LINK: i32 = 6044;
pub const MENU_ID_JOIN_CALL_LINK: i32 = 6045;
pub const MENU_ID_FOCUS_MESSAGE_FIELD: i32 = 6046;
pub const MENU_ID_ANNOUNCE_STATUS: i32 = 6047;
pub const MENU_ID_FOCUS_MESSAGES_LIST: i32 = 6048;
pub const MENU_ID_EMOJI_PANEL: i32 = 6049;
pub const MENU_ID_STICKER_PANEL: i32 = 6050;
pub const MENU_ID_NEXT_SPELLING_ERROR: i32 = 6051;
pub const MENU_ID_TOGGLE_ANNOUNCE: i32 = 6052;
pub const MENU_ID_CHAT_PRIVACY: i32 = 6041;
pub const MENU_ID_BUSINESS_CATALOG: i32 = 6060;
pub const MENU_ID_GO_TO_DATE: i32 = 6062;
pub const MENU_ID_SCHEDULE_MESSAGE: i32 = 6064;
pub const MENU_ID_SCHEDULED_MESSAGES: i32 = 6065;

const CONVERSATION_MENU_IDS: [i32; 24] = [
	MENU_ID_CHAT_INFO,
	MENU_ID_GO_TO_DATE,
	MENU_ID_BUSINESS_CATALOG,
	MENU_ID_SCHEDULE_MESSAGE,
	MENU_ID_SCHEDULED_MESSAGES,
	MENU_ID_SEARCH_CHAT,
	MENU_ID_PINNED_MESSAGES,
	MENU_ID_MUTE_CHAT,
	MENU_ID_ARCHIVE_CHAT,
	MENU_ID_PIN_CHAT,
	MENU_ID_LOCK_CHAT,
	MENU_ID_MARK_UNREAD,
	MENU_ID_DISAPPEARING,
	MENU_ID_CHAT_PRIVACY,
	MENU_ID_REPORT_CHAT,
	MENU_ID_BLOCK_CHAT,
	MENU_ID_STAR_MESSAGE,
	MENU_ID_FOCUS_MESSAGE_FIELD,
	MENU_ID_FOCUS_MESSAGES_LIST,
	MENU_ID_ANNOUNCE_STATUS,
	MENU_ID_TOGGLE_ANNOUNCE,
	MENU_ID_EMOJI_PANEL,
	MENU_ID_STICKER_PANEL,
	MENU_ID_NEXT_SPELLING_ERROR,
];

#[derive(Clone, Copy)]
enum ReturnPane {
	Nav,
	Chats,
	Messages,
}

enum ReturnSpot {
	Pane { pane: ReturnPane, index: Option<u32>, chat: Option<String> },
	Control(usize),
}

fn restore_focus(handle: usize) -> bool {
	let hwnd = windows::Win32::Foundation::HWND(handle as *mut std::ffi::c_void);
	unsafe {
		windows::Win32::UI::WindowsAndMessaging::IsWindow(Some(hwnd)).as_bool()
			&& windows::Win32::UI::Input::KeyboardAndMouse::SetFocus(Some(hwnd)).is_ok()
	}
}

/// Shows the selected message's own content (no sender, time or reply wrapper:
/// see `ChatMessage::body_text`) in a read-only multi-line field. A list row's
/// text can be read by NVDA but not selected or copied, so this gives a normal
/// field to browse and Ctrl+C from without letting anyone change the message.
///
/// Uses `Rich2` and `DontWrap` with CRLF newlines so screen readers (NVDA/JAWS)
/// can navigate paragraph by paragraph with Ctrl+Up/Down and lines aren't
/// artificially split by word wrapping.
fn show_message_text(
	parent: &Frame,
	list: &crate::ui::message_list::MessageList,
	state: &std::cell::RefCell<crate::AppState>,
) {
	let Some(idx) = list.get_selection() else { return };
	let text = state.borrow().display_items.get(idx as usize).map(|item| item.body_text());
	let Some(text) = text else { return };
	let text = text.replace("\r\n", "\n").replace('\n', "\r\n");
	let dlg = Dialog::builder(parent, "Message Text")
		.with_size(480, 320)
		.with_style(DialogStyle::DefaultDialogStyle | DialogStyle::ResizeBorder)
		.build();
	let dlg_panel = Panel::builder(&dlg).build();
	let dlg_sizer = BoxSizer::builder(Orientation::Vertical).build();

	let text_view = TextCtrl::builder(&dlg_panel)
		.with_style(TextCtrlStyle::MultiLine | TextCtrlStyle::ReadOnly | TextCtrlStyle::Rich2 | TextCtrlStyle::DontWrap)
		.build();
	text_view.set_value(&text);
	dlg_sizer.add(&text_view, 1, SizerFlag::Expand | SizerFlag::All, 8);

	let close_btn = Button::builder(&dlg_panel).with_id(wxdragon::id::ID_OK).with_label("Close").build();
	dlg_sizer.add(&close_btn, 0, SizerFlag::AlignRight | SizerFlag::All, 8);
	dlg_panel.set_sizer(dlg_sizer, true);

	let dlg_esc = dlg;
	text_view.on_key_down(keys::escape_cancels(dlg_esc));

	text_view.set_focus();
	text_view.set_selection(0, 0);
	dlg.show_modal();
	// The base Dialog has no Drop, so a dialog built per use must be
	// destroyed or it (and every closure bound to it) lives on hidden.
	dlg.destroy();
}

/// The chat management actions that work on the chat focused in the chat
/// list as well as on the open one, like WA Web's chat list shortcuts.
const CHAT_LIST_ACTIONS: [(i32, &str); 5] = [
	(MENU_ID_MUTE_CHAT, "mute_chat"),
	(MENU_ID_ARCHIVE_CHAT, "archive_chat"),
	(MENU_ID_PIN_CHAT, "pin_chat"),
	(MENU_ID_LOCK_CHAT, "lock_chat"),
	(MENU_ID_MARK_UNREAD, "mark_unread"),
];

/// The chat the chat list's focus is on, when the list has focus and shows
/// chats (not status, starred messages or calls).
fn chat_list_focus_chat(s: &crate::AppState, list: &ListBox) -> Option<String> {
	if matches!(s.active_tab, 1 | 2 | 6) || !list.has_focus() {
		return None;
	}
	let idx = list.get_selection()? as usize;
	if s.filtered_message_ids.get(idx).is_some_and(|id| !id.is_empty()) {
		return None;
	}
	s.filtered_chats.get(idx).filter(|jid| s.chats.contains(*jid)).cloned()
}

pub(crate) fn conversation_menu_chat(s: &crate::AppState) -> Option<&str> {
	if s.active_tab == 1 || s.active_tab == 2 {
		return None;
	}
	s.selected_chat.as_deref()
}

const GO_MENU_ITEMS: [(i32, &str); 7] = [
	(MENU_ID_NAV_CONVERSATIONS, "Jump to the chat list"),
	(MENU_ID_NAV_STATUS, "Jump to Status"),
	(MENU_ID_NAV_STARRED, "Jump to Starred Messages"),
	(MENU_ID_NAV_ARCHIVED, "Jump to Archived Chats"),
	(MENU_ID_NAV_COMMUNITIES, "Jump to Communities"),
	(MENU_ID_NAV_CHANNELS, "Jump to the channels you follow"),
	(MENU_ID_NAV_CALLS, "Jump to Calls"),
];

fn reorder_go_menu(menu_bar: &MenuBar, order: &[usize]) {
	for (pos, &tab) in order.iter().enumerate() {
		let (id, help) = GO_MENU_ITEMS[tab];
		let Some((item, menu)) = menu_bar.find_item_and_menu(id) else { continue };
		let label = item.get_label();
		menu.delete(id);
		menu.insert(pos, id, &label, help, ItemKind::Normal);
	}
}

enum ConversationEntry {
	Item(i32, Option<&'static str>, &'static str),
	Separator,
}

use ConversationEntry::{Item, Separator};

const CONVERSATION_MENU: [ConversationEntry; 29] = [
	Item(MENU_ID_CHAT_INFO, None, "Open info for the open chat"),
	Item(MENU_ID_FOCUS_MESSAGE_FIELD, None, "Move focus to the message box, or back to where you were"),
	Item(MENU_ID_FOCUS_MESSAGES_LIST, None, "Move focus to the messages list"),
	Item(MENU_ID_ANNOUNCE_STATUS, None, "Say the open chat's status: online, typing and so on"),
	Item(MENU_ID_TOGGLE_ANNOUNCE, None, "Turn reading new messages aloud on or off for the open chat"),
	Item(MENU_ID_EMOJI_PANEL, None, "Pick an emoji to put in the message box"),
	Item(MENU_ID_STICKER_PANEL, None, "Pick a sticker to send: favorites, recent ones or WhatsApp's own packs"),
	Item(MENU_ID_NEXT_SPELLING_ERROR, None, "Select the next misspelled word in the message box and offer its fixes"),
	Item(MENU_ID_SEARCH_CHAT, None, "Search within the open chat"),
	Item(MENU_ID_GO_TO_DATE, Some("Go to &Date..."), "Jump to the messages of a date in the open chat"),
	Item(MENU_ID_PINNED_MESSAGES, Some("Pi&nned Messages..."), "See and go to the open chat's pinned messages"),
	Item(
		MENU_ID_BUSINESS_CATALOG,
		Some("Business Catalo&gue..."),
		"Browse the products of the business you're chatting with",
	),
	Separator,
	Item(MENU_ID_SCHEDULE_MESSAGE, None, "Pick a date and time for the message in the message box to be sent"),
	Item(MENU_ID_SCHEDULED_MESSAGES, None, "See, send now or delete the open chat's scheduled messages"),
	Separator,
	Item(MENU_ID_MUTE_CHAT, None, "Mute the open chat"),
	Item(MENU_ID_ARCHIVE_CHAT, None, "Archive the open chat"),
	Item(MENU_ID_PIN_CHAT, None, "Pin the open chat"),
	Item(MENU_ID_LOCK_CHAT, None, "Lock or unlock the open chat"),
	Item(MENU_ID_MARK_UNREAD, None, "Toggle read/unread on the open chat"),
	Separator,
	Item(
		MENU_ID_DISAPPEARING,
		Some("&Disappearing Messages..."),
		"Turn disappearing messages on or off for the open chat",
	),
	Item(
		MENU_ID_CHAT_PRIVACY,
		Some("Ad&vanced Chat Privacy..."),
		"Turn advanced chat privacy on or off for the open chat",
	),
	Separator,
	Item(MENU_ID_REPORT_CHAT, Some("&Report..."), "Report the open chat's contact or group to WhatsApp"),
	Item(MENU_ID_BLOCK_CHAT, Some("&Block or Unblock Contact"), "Block or unblock the open chat's contact"),
	Separator,
	Item(MENU_ID_STAR_MESSAGE, None, "Star the selected message"),
];

fn conversation_item_applies(s: &crate::AppState, jid: &str, id: i32) -> bool {
	let is_person = jid.ends_with("@s.whatsapp.net") || jid.ends_with("@lid");
	let is_channel = jid.ends_with("@newsletter");
	let is_broadcast = jid.ends_with("@broadcast");
	let is_group = jid.ends_with("@g.us");
	match id {
		// WA Web's isScheduledMessagesAvailableFor: people and groups, not
		// bots, channels or broadcasts, when the server lets this account
		// schedule.
		MENU_ID_SCHEDULE_MESSAGE | MENU_ID_SCHEDULED_MESSAGES => {
			(is_person || is_group) && !jid.ends_with("@bot") && s.scheduled_messages_enabled
		}
		MENU_ID_BUSINESS_CATALOG => is_person && s.business_jids.contains(jid),
		MENU_ID_BLOCK_CHAT => is_person,
		MENU_ID_PINNED_MESSAGES => !is_broadcast,
		MENU_ID_DISAPPEARING | MENU_ID_STAR_MESSAGE => !is_channel,
		MENU_ID_REPORT_CHAT | MENU_ID_CHAT_PRIVACY => !is_channel && !is_broadcast,
		_ => true,
	}
}

fn conversation_entries(applies: impl Fn(i32) -> bool) -> Vec<&'static ConversationEntry> {
	let mut shown: Vec<&ConversationEntry> = Vec::new();
	for entry in &CONVERSATION_MENU {
		match entry {
			Item(id, ..) if applies(*id) => shown.push(entry),
			Separator if matches!(shown.last(), Some(Item(..))) => shown.push(entry),
			_ => {}
		}
	}
	if matches!(shown.last(), Some(Separator)) {
		shown.pop();
	}
	shown
}

fn fill_conversation_menu(menu: &Menu, entries: &[&ConversationEntry], keymap: &crate::keymap::Keymap) {
	while let Some(item) = menu.find_item_by_position(0) {
		menu.delete_item(&item);
	}
	for entry in entries {
		match entry {
			Item(id, Some(label), help) => {
				menu.append(*id, label, help, ItemKind::Normal);
			}
			Item(id, None, help) => {
				menu.append(*id, &keymap.menu_label_for(*id), help, ItemKind::Normal);
			}
			Separator => menu.append_separator(),
		}
	}
}

fn conversation_menu_layout(menu: &Menu) -> Vec<i32> {
	menu.get_menu_items().iter().map(|item| item.get_item_id().max(0)).collect()
}

fn fit_conversation_menu(s: &crate::AppState, menu_bar: &MenuBar, jid: &str) {
	let Some((_, menu)) = menu_bar.find_item_and_menu(MENU_ID_CHAT_INFO) else { return };
	let entries = conversation_entries(|id| conversation_item_applies(s, jid, id));
	let wanted: Vec<i32> = entries
		.iter()
		.map(|e| match e {
			Item(id, ..) => *id,
			Separator => 0,
		})
		.collect();
	if conversation_menu_layout(&menu) != wanted {
		fill_conversation_menu(&menu, &entries, &s.keymap);
	}
}

/// Greys out what doesn't apply right now, and leaves out of the
/// Conversation menu what never applies to the open chat. A disabled item's
/// shortcut does nothing either, so this runs whenever the tab or the open
/// chat changes, not only when a menu opens.
pub(crate) fn refresh_menu_bar(s: &crate::AppState, frame: &Frame) {
	let Some(menu_bar) = frame.get_menu_bar() else { return };
	let chat = conversation_menu_chat(s);
	if let Some(jid) = chat {
		fit_conversation_menu(s, &menu_bar, jid);
	}
	// The chat list's own actions (mute, archive, pin, lock, mark unread)
	// also work on the chat focused in the list, so they stay enabled
	// wherever the list shows chats. Windows swallows the shortcut of a
	// greyed item without passing the key on, so greying them made
	// Ctrl+Shift+U and the rest do nothing in the list with no chat open.
	let list_shows_chats = !matches!(s.active_tab, 1 | 2 | 6);
	for id in CONVERSATION_MENU_IDS {
		let list_action = CHAT_LIST_ACTIONS.iter().any(|(action, _)| *action == id);
		menu_bar.enable_item(id, chat.is_some() || (list_action && list_shows_chats));
	}
}

pub fn build_main_window() -> WindowParts {
	crate::ui::dialog_look::round_window_corners();
	let frame = Frame::builder().with_title("whatSnap").with_size(Size::new(1180, 760)).build();
	frame.set_min_size(Size::new(900, 600));
	frame.set_icon(&theme::build_app_icon(32));
	wxdragon::app::set_top_window(&frame);
	frame.create_status_bar(1, 0, wxdragon::id::ID_ANY as i32, "status_bar");
	frame.set_status_text("Status: Initializing...", 0);
	theme::style_content(&frame);

	let main_panel = Panel::builder(&frame).build();
	theme::style_content(&main_panel);
	let main_sizer = BoxSizer::builder(Orientation::Horizontal).build();

	let nav_panel = Panel::builder(&main_panel).build();
	theme::style_sidebar(&nav_panel);
	nav_panel.set_min_size(Size::new(nav_panel.get_char_width() * 12 + 8, -1));
	let nav_sizer = BoxSizer::builder(Orientation::Vertical).build();
	let nav_label = StaticText::builder(&nav_panel).with_label("Main Navigation").build();
	theme::style_sidebar_heading(&nav_label);
	let tab_order = crate::settings::Settings::load().tab_order();
	let nav_list = ListBox::builder(&nav_panel)
		.with_choices(tab_order.iter().map(|&t| crate::settings::TAB_NAMES[t].to_string()).collect())
		.with_style(ListBoxStyle::from_bits_retain(crate::ui::chat_rows::OWNER_DRAW))
		.build();
	nav_list.set_font(&theme::Fonts::body());
	theme::style_sidebar(&nav_list);
	crate::ui::owner_draw::install_nav(&nav_list);
	// It still names the list for a screen reader; there's no room to show it.
	nav_label.set_min_size(Size::new(1, 1));
	nav_list.set_selection(tab_order.iter().position(|&t| t == 0).unwrap_or(0) as u32, true);
	nav_sizer.add(&nav_label, 0, SizerFlag::Left, 0);
	nav_sizer.add(&nav_list, 1, SizerFlag::Expand | SizerFlag::Top, 6);
	if theme::use_custom_colours() {
		let footer = crate::ui::owner_draw::install_nav_footer(&nav_panel, &frame, MENU_ID_SETTINGS);
		nav_sizer.add_spacer(footer);
	}

	nav_panel.set_sizer(nav_sizer, true);

	let account_menu = Menu::builder()
		.append_item(MENU_ID_SETTINGS, &default_menu_label(MENU_ID_SETTINGS), "Open settings")
		.append_item(
			MENU_ID_ACCOUNTS,
			&default_menu_label(MENU_ID_ACCOUNTS),
			"Switch between, add and remove WhatsApp accounts",
		)
		.append_item(
			MENU_ID_RECONNECT,
			&default_menu_label(MENU_ID_RECONNECT),
			"Retry connecting to WhatsApp right away",
		)
		.append_item(MENU_ID_BLOCKED_CONTACTS, "&Blocked Contacts...", "See and change who you have blocked")
		.append_item(MENU_ID_CHECK_UPDATES, "Check for &Updates...", "See if a newer version of whatSnap is out")
		.append_separator()
		.append_item(MENU_ID_LOGOUT, "Log &Out", "Log out of WhatsApp on this device")
		.build();
	let go_menu = Menu::builder().build();
	for &tab in &tab_order {
		let (id, help) = GO_MENU_ITEMS[tab];
		go_menu.append(id, &default_menu_label(id), help, ItemKind::Normal);
	}
	let chats_menu = Menu::builder()
		.append_item(MENU_ID_NEW_CHAT, &default_menu_label(MENU_ID_NEW_CHAT), "Start a new chat")
		.append_item(MENU_ID_NEW_GROUP, &default_menu_label(MENU_ID_NEW_GROUP), "Start a new group")
		.append_item(MENU_ID_NEW_COMMUNITY, "New C&ommunity...", "Start a community to bring groups together")
		.append_separator()
		.append_item(MENU_ID_SEARCH_GLOBAL, &default_menu_label(MENU_ID_SEARCH_GLOBAL), "Search chats")
		.append_item(MENU_ID_MANAGE_LISTS, "Manage &Lists...", "Make, rename or delete your chat lists")
		.build();
	let conversation_menu = Menu::builder().build();
	fill_conversation_menu(&conversation_menu, &CONVERSATION_MENU.iter().collect::<Vec<_>>(), &Default::default());
	let call_menu = Menu::builder()
		.append_item(
			MENU_ID_CALL_MUTE,
			&default_menu_label(MENU_ID_CALL_MUTE),
			"Mute/unmute the microphone during a call",
		)
		.append_item(MENU_ID_CALL_END, &default_menu_label(MENU_ID_CALL_END), "End the active call")
		.append_separator()
		.append_item(MENU_ID_CREATE_CALL_LINK, "&Create Call Link...", "Make a link anyone can use to join a call")
		.append_item(MENU_ID_JOIN_CALL_LINK, "&Join Call Link...", "Join a call from a link someone shared")
		.build();
	let menu_bar = MenuBar::builder()
		.append(account_menu, "&Account")
		.append(go_menu, "&Go")
		.append(chats_menu, "&Chats")
		.append(conversation_menu, "C&onversation")
		.append(call_menu, "Call&s")
		.build();
	frame.set_menu_bar(menu_bar);

	let content_panel = Panel::builder(&main_panel).build();
	theme::style_content(&content_panel);
	let content_sizer = BoxSizer::builder(Orientation::Vertical).build();

	let login_panel = Dialog::builder(&frame, "WhatsApp Login").with_size(500, 360).build();
	theme::style_content(&login_panel);
	let login_sizer = BoxSizer::builder(Orientation::Vertical).build();

	// QR Code Panel (embedded inside login_panel)
	let qr_panel = Panel::builder(&login_panel).build();
	theme::style_content(&qr_panel);
	let qr_sizer = BoxSizer::builder(Orientation::Vertical).build();
	let qrcode_instructions = StaticText::builder(&qr_panel).with_label("Scan this QR Code with WhatsApp:").build();
	theme::style_heading(&qrcode_instructions);
	let empty_bitmap = wxdragon::bitmap::Bitmap::from_rgba(&[255, 255, 255, 255], 1, 1).unwrap();
	let qrcode_display = StaticBitmap::builder(&qr_panel)
		.with_bitmap(Some(empty_bitmap))
		.with_scale_mode(Some(ScaleMode::AspectFit))
		.build();

	let qrcode_announcer = StaticText::builder(&qr_panel).with_label("").build();
	live_region::set_live_region(&qrcode_announcer);
	theme::style_muted_label(&qrcode_announcer);

	let qrcode_reload_btn = Button::builder(&qr_panel).with_label("Reload QR Code").build();
	theme::style_primary_button(&qrcode_reload_btn);
	qrcode_reload_btn.show(false);

	let qrcode_cancel_btn = Button::builder(&qr_panel).with_label("Back to Phone Number Login").build();
	qrcode_cancel_btn.set_font(&theme::Fonts::body());
	qr_sizer.add(&qrcode_instructions, 0, SizerFlag::All | SizerFlag::AlignCentre, 10);
	qr_sizer.add(&qrcode_display, 0, SizerFlag::Expand | SizerFlag::All, 10);
	qr_sizer.add(&qrcode_reload_btn, 0, SizerFlag::All | SizerFlag::AlignCentre, 6);
	qr_sizer.add(&qrcode_announcer, 0, SizerFlag::Left, 0);
	qr_sizer.add(&qrcode_cancel_btn, 0, SizerFlag::All | SizerFlag::AlignCentre, 10);
	qr_panel.set_sizer(qr_sizer, true);
	qr_panel.show(false);

	// Phone Number Login Panel (embedded inside login_panel)
	let phone_panel = Panel::builder(&login_panel).build();
	theme::style_content(&phone_panel);
	let phone_sizer = BoxSizer::builder(Orientation::Vertical).build();
	let country_label = StaticText::builder(&phone_panel).with_label("Country:").build();
	theme::style_muted_label(&country_label);
	let mut choices = Vec::new();
	for &(name, _, _) in crate::countries::COUNTRIES {
		choices.push(name.to_string());
	}
	let country_combo =
		ComboBox::builder(&phone_panel).with_choices(choices).with_style(ComboBoxStyle::ReadOnly).build();
	let country_idx = crate::countries::default_index();
	country_combo.set_selection(country_idx as u32);
	country_combo.set_font(&theme::Fonts::body());

	let phone_label = StaticText::builder(&phone_panel).with_label("Enter phone number:").build();
	theme::style_muted_label(&phone_label);
	let phone_field = TextCtrl::builder(&phone_panel)
		.with_value(&format!("+{} ", crate::countries::COUNTRIES[country_idx].2))
		.build();
	phone_field.set_font(&theme::Fonts::body());
	let pair_btn = Button::builder(&phone_panel).with_label("Continue").build();
	theme::style_primary_button(&pair_btn);
	let switch_to_qr_btn = Button::builder(&phone_panel).with_label("Connect with QR Code").build();
	switch_to_qr_btn.set_font(&theme::Fonts::body());

	phone_sizer.add(&country_label, 0, SizerFlag::Left | SizerFlag::Top, 10);
	phone_sizer.add(&country_combo, 0, SizerFlag::Expand | SizerFlag::All, 10);
	phone_sizer.add(&phone_label, 0, SizerFlag::Left | SizerFlag::Top, 10);
	phone_sizer.add(&phone_field, 0, SizerFlag::Expand | SizerFlag::All, 10);
	phone_sizer.add(&pair_btn, 0, SizerFlag::All | SizerFlag::AlignCentre, 10);
	phone_sizer.add(&switch_to_qr_btn, 0, SizerFlag::All | SizerFlag::AlignCentre, 10);
	phone_panel.set_sizer(phone_sizer, true);
	phone_panel.show(true);

	// Pairing Code Panel (embedded inside login_panel)
	let pairing_panel = Panel::builder(&login_panel).build();
	theme::style_content(&pairing_panel);
	let p_sizer = BoxSizer::builder(Orientation::Vertical).build();
	let pairing_instructions =
		StaticText::builder(&pairing_panel).with_label("Enter this pairing code in WhatsApp on your phone:").build();
	theme::style_heading(&pairing_instructions);
	let pairing_code_display =
		TextCtrl::builder(&pairing_panel).with_style(TextCtrlStyle::ReadOnly | TextCtrlStyle::Centre).build();
	pairing_code_display.set_font(&theme::Fonts::heading());

	let pairing_announcer = StaticText::builder(&pairing_panel).with_label("").build();
	live_region::set_live_region(&pairing_announcer);

	// No ID_CANCEL on the Back buttons: the dialog would treat a press as
	// Cancel and hide the whole login window, which now holds all three views.
	let pairing_cancel_btn = Button::builder(&pairing_panel).with_label("Back to Phone Number Login").build();
	pairing_cancel_btn.set_font(&theme::Fonts::body());
	p_sizer.add(&pairing_instructions, 0, SizerFlag::All | SizerFlag::AlignCentre, 10);
	p_sizer.add(&pairing_code_display, 0, SizerFlag::Expand | SizerFlag::All, 10);
	p_sizer.add(&pairing_announcer, 0, SizerFlag::Left, 0);
	pairing_announcer.show(false);
	p_sizer.add(&pairing_cancel_btn, 0, SizerFlag::All | SizerFlag::AlignCentre, 10);
	pairing_panel.set_sizer(p_sizer, true);
	pairing_panel.show(false);

	login_sizer.add(&phone_panel, 1, SizerFlag::Expand | SizerFlag::All, 5);
	login_sizer.add(&qr_panel, 1, SizerFlag::Expand | SizerFlag::All, 5);
	login_sizer.add(&pairing_panel, 1, SizerFlag::Expand | SizerFlag::All, 5);
	login_panel.set_sizer(login_sizer, true);

	let qrcode_dialog = login_panel;
	let pairing_dialog = login_panel;

	let new_conv_dialog = crate::ui::dialogs::new_conversation::build_new_conversation_dialog(&frame);
	let new_contact_dialog = crate::ui::dialogs::new_contact::build_new_contact_dialog(&frame);
	let poll_dialog = crate::ui::dialogs::poll_dialog::build_poll_dialog(&frame);
	let new_group_dialog = crate::ui::dialogs::new_group::build_new_group_dialog(&frame);
	let add_members_dialog = crate::ui::dialogs::add_members::build_add_members_dialog(&frame);
	let attach_contact_dialog = crate::ui::dialogs::attach_contact::build_attach_contact_dialog(&frame);
	let new_group_contacts_timer = Rc::new(Timer::new(&new_group_dialog.dialog));
	let add_members_contacts_timer = Rc::new(Timer::new(&add_members_dialog.dialog));
	let attach_contact_timer = Rc::new(Timer::new(&attach_contact_dialog.dialog));
	let attachment_dialog = crate::ui::dialogs::attachment_dialog::build_attachment_dialog(&frame);
	let call_parts = crate::ui::call_window::build_call_window();
	let conversation_data_dialog = crate::ui::conversation_data::build_conversation_data_dialog(&frame);
	let group_data_dialog = crate::ui::group_data::build_group_data_dialog(&frame);
	let settings_dialog = crate::ui::dialogs::settings_dialog::build_settings_dialog(&frame);
	let tray_icon = Rc::new(TaskBarIcon::builder().build());

	let conv_panel = crate::ui::conversations::build_conversations_panel(&content_panel);

	content_sizer.add(&conv_panel.panel, 1, SizerFlag::Expand, 0);
	conv_panel.panel.show(true);

	nav_panel.show(false);
	content_panel.show(false);
	login_panel.show(false);

	content_panel.set_sizer(content_sizer, true);

	main_sizer.add(&nav_panel, 0, SizerFlag::Expand, 0);
	main_sizer.add(&content_panel, 1, SizerFlag::Expand, 0);
	main_panel.set_sizer(main_sizer, true);

	let frame_sizer = BoxSizer::builder(Orientation::Vertical).build();
	frame_sizer.add(&main_panel, 1, SizerFlag::Expand, 0);
	frame.set_sizer(frame_sizer, true);

	set_dialog_owner(&login_panel, &frame);
	set_dialog_owner(&new_conv_dialog.dialog, &frame);
	set_dialog_owner(&new_contact_dialog.dialog, &frame);
	set_dialog_owner(&poll_dialog.dialog, &frame);
	set_dialog_owner(&new_group_dialog.dialog, &frame);
	set_dialog_owner(&add_members_dialog.dialog, &frame);
	set_dialog_owner(&attach_contact_dialog.dialog, &frame);
	set_dialog_owner(&attachment_dialog.dialog, &frame);
	set_dialog_owner(&conversation_data_dialog.dialog, &frame);
	set_dialog_owner(&conversation_data_dialog.details_dialog, &conversation_data_dialog.dialog);
	set_dialog_owner(&group_data_dialog.dialog, &frame);
	set_dialog_owner(&settings_dialog.dialog, &frame);

	WindowParts {
		frame,
		nav_list,
		content_panel,
		login_panel,
		main_panel,
		nav_panel,
		qrcode_dialog,
		qrcode_instructions,
		qrcode_display,
		qrcode_announcer,
		qrcode_reload_btn,
		qrcode_cancel_btn,
		qr_panel,
		phone_panel,
		country_combo,
		phone_field,
		pair_btn,
		switch_to_qr_btn,
		pairing_dialog,
		pairing_code_display,
		pairing_announcer,
		pairing_cancel_btn,
		pairing_panel,
		new_conv_dialog,
		new_contact_dialog,
		poll_dialog,
		new_group_dialog,
		add_members_dialog,
		attach_contact_dialog,
		new_group_contacts_timer,
		add_members_contacts_timer,
		attach_contact_timer,
		attachment_dialog,
		call_parts,
		conversation_data_dialog,
		group_data_dialog,
		conversations_parts: conv_panel,
		settings_dialog,
		tray_icon,
	}
}

pub fn bind_input_handlers(
	parts: &WindowParts,
	ui_tx: UiCommandSender,
	state: &std::rc::Rc<std::cell::RefCell<crate::AppState>>,
) {
	let tx_pair = ui_tx.clone();
	let phone_ctrl = parts.phone_field;
	let combo = parts.country_combo;
	combo.on_selection_changed(move |event| {
		if let Some(idx) = event.get_selection() {
			let code = crate::countries::COUNTRIES[idx as usize].2;
			phone_ctrl.set_value(&format!("+{} ", code));
		}
	});

	let phone_ctrl2 = parts.phone_field;
	parts.pair_btn.on_click(move |_| {
		let phone = phone_ctrl2.get_value();
		let clean_phone: String = phone.chars().filter(|c| c.is_ascii_digit()).collect();
		if !clean_phone.is_empty() {
			let _ = tx_pair.send(UiCommand::RequestPairing(clean_phone));
		}
	});

	let login_ctl = LoginControls::from_parts(parts);

	let tx_qr = ui_tx.clone();
	let login_ctl_qr = login_ctl;
	let qr_instructions_switch = parts.qrcode_instructions;
	let qr_reload_switch = parts.qrcode_reload_btn;
	parts.switch_to_qr_btn.on_click(move |_| {
		qr_instructions_switch.set_label("Loading QR Code...");
		qr_reload_switch.show(false);
		login_ctl_qr.switch_view(LoginView::Qr);
		let _ = tx_qr.send(UiCommand::RequestQr);
	});

	let login_ctl_cancel_qr = login_ctl;
	let tx_cancel_qr = ui_tx.clone();
	let qr_instructions_cancel = parts.qrcode_instructions;
	let qr_reload_cancel = parts.qrcode_reload_btn;
	parts.qrcode_cancel_btn.on_click(move |_| {
		qr_instructions_cancel.set_label("Scan this QR Code with WhatsApp:");
		qr_reload_cancel.show(false);
		login_ctl_cancel_qr.switch_view(LoginView::Phone);
		let _ = tx_cancel_qr.send(UiCommand::CancelQr);
	});

	let tx_reload_qr = ui_tx.clone();
	let qr_instructions_reload = parts.qrcode_instructions;
	let qr_reload_self = parts.qrcode_reload_btn;
	let login_p_reload = parts.login_panel;
	let qr_cancel_reload = parts.qrcode_cancel_btn;
	parts.qrcode_reload_btn.on_click(move |_| {
		qr_instructions_reload.set_label("Loading new QR Code...");
		qr_reload_self.show(false);
		login_p_reload.layout();
		qr_cancel_reload.set_focus();
		let _ = tx_reload_qr.send(UiCommand::RequestQr);
	});

	let login_ctl_cancel_p = login_ctl;
	let tx_cancel_pairing = ui_tx.clone();
	parts.pairing_cancel_btn.on_click(move |_| {
		login_ctl_cancel_p.switch_view(LoginView::Phone);
		let _ = tx_cancel_pairing.send(UiCommand::CancelPairing);
	});

	// One login window holds the phone, QR and pairing views, so Escape goes
	// back to the phone number view from the other two (and cancels that
	// attempt) instead of closing the window.
	let login_ctl_esc = login_ctl;
	let tx_esc_login = ui_tx.clone();
	let qr_instructions_esc = parts.qrcode_instructions;
	let qr_reload_esc = parts.qrcode_reload_btn;
	parts.login_panel.bind_internal(EventType::CHAR_HOOK, move |event| {
		let data = WindowEventData::new(event);
		if let WindowEventData::Keyboard(ref key) = data
			&& keys::is_escape(key.get_key_code())
		{
			if login_ctl_esc.qr_panel.is_shown() {
				qr_instructions_esc.set_label("Scan this QR Code with WhatsApp:");
				qr_reload_esc.show(false);
				login_ctl_esc.switch_view(LoginView::Phone);
				let _ = tx_esc_login.send(UiCommand::CancelQr);
				data.skip(false);
				return;
			}
			if login_ctl_esc.pairing_panel.is_shown() {
				login_ctl_esc.switch_view(LoginView::Phone);
				let _ = tx_esc_login.send(UiCommand::CancelPairing);
				data.skip(false);
				return;
			}
		}
		data.skip(true);
	});

	let tx_send = ui_tx.clone();
	let msg_ctrl = parts.conversations_parts.message_field;
	let state_send_btn = state.clone();
	parts.conversations_parts.send_message_btn.on_click(move |_| {
		let text = msg_ctrl.get_value();
		if let Some(message_id) = state_send_btn.borrow().editing_message_id.clone() {
			let _ = tx_send.send(UiCommand::EditMessage { message_id, new_text: text });
		} else if !text.is_empty() {
			let _ = tx_send.send(UiCommand::SendMessage(text));
		}
	});

	// Context menu key / Shift+F10 on the Send button offers "Schedule
	// message...", where WA Web puts its schedule button.
	{
		let send_btn_ctx = parts.conversations_parts.send_message_btn;
		let tx_sched = ui_tx.clone();
		let state_sched = state.clone();
		send_btn_ctx.bind_internal(EventType::CONTEXT_MENU, move |_event| {
			let offer = {
				let s = state_sched.borrow();
				s.editing_message_id.is_none()
					&& s.selected_chat.as_deref().is_some_and(|jid| crate::ui_commands::scheduled::schedulable(&s, jid))
			};
			if !offer {
				return;
			}
			let schedule_id = 4110;
			let mut menu = Menu::builder()
				.append_item(schedule_id, "&Schedule message...", "Pick a date and time for this message to be sent")
				.build();
			let tx_pick = tx_sched.clone();
			menu.on_selected(move |evt| {
				if evt.get_id() == schedule_id {
					let _ = tx_pick.send(UiCommand::OpenScheduleDialog);
				}
			});
			send_btn_ctx.popup_menu(&mut menu, None);
			menu.destroy_menu();
		});
	}

	{
		let field = parts.conversations_parts.message_field;
		{
			let st = state.borrow();
			crate::ui::spell_check::configure(st.settings.spell_check, &st.settings.spell_language);
		}
		crate::ui::spell_check::install(&field);
	}
	let tx_typing = ui_tx.clone();
	let msg_ctrl_typing = parts.conversations_parts.message_field;
	let state_typing = state.clone();
	let conv_parts_typing = parts.conversations_parts;
	msg_ctrl_typing.on_text_changed(move |_| {
		let replace_emoji = state_typing.try_borrow().is_ok_and(|s| s.settings.replace_text_with_emoji);
		crate::ui::emoji_replace::text_changed(&msg_ctrl_typing, replace_emoji);
		let announce_spelling = state_typing.try_borrow().is_ok_and(|s| s.settings.spell_check_announce);
		crate::ui::spell_check::text_changed(
			&msg_ctrl_typing,
			announce_spelling.then_some(conv_parts_typing.messages_announcer),
		);
		crate::ui_commands::mentions::refresh(&state_typing, &conv_parts_typing);
		if let Ok(s) = state_typing.try_borrow() {
			crate::ui::chat_ui::update_composer_buttons(&s, &conv_parts_typing);
		}
		let text = msg_ctrl_typing.get_value();
		// set_value() fires this too - restoring a chat's saved draft on open,
		// or entering edit mode, is not the user typing, and mustn't tell the
		// chat "composing". Clearing the field still counts (it's how a
		// "composing" we did send gets taken back).
		if !text.is_empty() && !msg_ctrl_typing.has_focus() {
			return;
		}
		let typing_state = if text.is_empty() { "paused" } else { "composing" };
		let _ = tx_typing.send(UiCommand::SendTyping {
			jid: String::new(),
			state: typing_state.to_string(),
			media: String::new(),
		});
	});

	let state_column = state.clone();
	crate::ui::conversations::on_list_column_resized(move |width| {
		if let Ok(mut s) = state_column.try_borrow_mut() {
			s.settings.chat_list_width = width;
			s.settings.save();
		}
	});

	crate::ui::avatars::init(state.borrow().ws_tx.clone());
	let picked = |state: &Rc<std::cell::RefCell<crate::AppState>>, jids: fn(&crate::AppState) -> &Vec<String>| {
		let state = state.clone();
		move |index: usize| state.try_borrow().ok().and_then(|s| jids(&s).get(index).cloned())
	};
	crate::ui::dialog_look::contact_rows(
		&parts.new_conv_dialog.results_list,
		picked(state, |s| &s.contact_picker_current_jids),
	);
	crate::ui::dialog_look::contact_check_rows(
		&parts.new_group_dialog.contacts_listbox,
		picked(state, |s| &s.group_state.current_jids),
	);
	crate::ui::dialog_look::contact_check_rows(
		&parts.add_members_dialog.contacts_listbox,
		picked(state, |s| &s.add_members_state.current_jids),
	);
	crate::ui::dialog_look::contact_check_rows(
		&parts.attach_contact_dialog.contacts_listbox,
		picked(state, |s| &s.attach_contact_state.current_jids),
	);
	let state_rows = state.clone();
	crate::ui::chat_rows::install(&parts.conversations_parts.conversations_list, move |index, text| {
		let Ok(s) = state_rows.try_borrow() else {
			wxdragon::call_after(Box::new(crate::ui::chat_rows::refresh));
			return None;
		};
		// The row's chat is only known while the list shows the rows the app
		// last gave it; any other time (mid-way through a switch of tab, say)
		// it's drawn from its text alone, with no one's picture, so a picture
		// never lands on someone else's row.
		let in_step = s.displayed_chat_labels.get(index).is_some_and(|l| l == text);
		let jid = s.filtered_chats.get(index).filter(|_| in_step).map_or("", String::as_str);
		let chat_tab = in_step && !matches!(s.active_tab, 1 | 2 | 6);
		// A message the chat list's search found, under its Messages heading.
		if let Some(id) = s.filtered_message_ids.get(index).filter(|id| chat_tab && !id.is_empty()) {
			return Some(match s.message_search.hits.iter().find(|h| h.message_id == *id) {
				Some(hit) => crate::ui::chat_rows::RowArt {
					jid: hit.jid.clone(),
					title: hit.chat_name.clone(),
					time: hit.date.clone(),
					preview_prefix: format!("{}: ", hit.sender),
					preview: hit.body.clone(),
					..Default::default()
				},
				None => crate::ui::chat_rows::RowArt::from_text(jid, text),
			});
		}
		match s.chat_row_art.get(jid) {
			Some(art) if chat_tab => {
				let mut art = art.clone();
				// Someone typing, as WhatsApp shows it in place of the last
				// message (a typing that never got its "stopped" fades out).
				let fresh = s.typing_timestamps.get(jid).is_some_and(|t| t.elapsed().as_secs() < 25);
				art.typing = match s.typing_announce_state.get(jid) {
					Some(2) if fresh => Some("recording audio…"),
					Some(_) if fresh => Some("typing…"),
					_ => None,
				};
				// An unsent draft, in place of the last message; typing wins.
				if art.typing.is_none()
					&& let Some(draft) = crate::ui::message_display::shown_draft(&s, jid)
				{
					art.preview_prefix = "Draft: ".to_string();
					art.preview = draft;
					art.icon = None;
					art.ticks = None;
					art.draft = true;
				}
				Some(art)
			}
			_ if chat_tab && jid.is_empty() => {
				Some(crate::ui::chat_rows::RowArt { title: text.to_string(), heading: true, ..Default::default() })
			}
			_ if in_step && s.active_tab == 1 => {
				let segments = s
					.status_groups
					.iter()
					.find(|g| !jid.is_empty() && (g.poster_jid == jid || (jid == "me" && g.from_me)))
					.map(|g| {
						let unseen = if g.from_me { 0 } else { g.items.iter().filter(|it| !it.viewed).count() };
						(g.items.len().min(u16::MAX as usize) as u16, unseen.min(u16::MAX as usize) as u16)
					})
					.unwrap_or((0, 0));
				Some(crate::ui::chat_rows::RowArt::for_status(jid, text, segments))
			}
			_ if in_step && s.active_tab == 2 => {
				if jid.is_empty() {
					Some(crate::ui::chat_rows::RowArt {
						title: text.to_string(),
						heading: true,
						..Default::default()
					})
				} else if let Some(msg_id) = s.filtered_starred_message_ids.get(index)
					&& let Some(entry) = s.starred_entries.iter().find(|e| &e.message_id == msg_id)
				{
					let time_str = if entry.timestamp > 0 {
						if let Some(local_dt) = whatsnap::LocalTime::from_unix(entry.timestamp) {
							let now_dt = whatsnap::LocalTime::now();
							let msg_date = local_dt.date();
							let today = now_dt.date();
							let yesterday = today.add_days(-1);
							let num_days_ago = today.days_since(msg_date);
							if msg_date == today {
								local_dt.format("%I:%M %p")
							} else if msg_date == yesterday {
								"Yesterday".to_string()
							} else if num_days_ago > 1 && num_days_ago < 7 {
								local_dt.format("%A")
							} else {
								local_dt.format("%d/%m/%Y")
							}
						} else {
							String::new()
						}
					} else {
						String::new()
					};
					let (icon, body) = crate::ui::chat_rows::starred_media_preview(&entry.media_type, &entry.text);
					Some(crate::ui::chat_rows::RowArt::for_starred(
						jid,
						&entry.chat_name,
						&entry.sender_name,
						entry.from_me,
						&body,
						&time_str,
						icon,
					))
				} else {
					Some(crate::ui::chat_rows::RowArt::from_text(jid, text))
				}
			}
			_ if in_step && s.active_tab == 6 => Some(crate::ui::chat_rows::RowArt::for_call(jid, text)),
			_ => Some(crate::ui::chat_rows::RowArt::from_text(jid, text)),
		}
	});

	let (messages_list, select_list) =
		(parts.conversations_parts.messages_list, parts.conversations_parts.messages_select_list);
	crate::ui::thumbnails::init(state.borrow().ws_tx.clone(), move || {
		messages_list.refresh(false, None);
		select_list.refresh(false, None);
		crate::ui::owner_draw::refresh_media_lists();
	});
	let state_members = state.clone();
	crate::ui::owner_draw::install_people_list(&parts.group_data_dialog.members_list, move |index| {
		state_members.try_borrow().ok()?.group_data_visible_member_jids.get(index).cloned()
	});
	for group in [false, true] {
		let list = if group { &parts.group_data_dialog.media_list } else { &parts.conversation_data_dialog.media_list };
		let state_media = state.clone();
		crate::ui::owner_draw::install_media_list(list, move |index| {
			let s = state_media.try_borrow().ok()?;
			let media = if group { &s.group_data_media } else { &s.conversation_data_media };
			media.get(index).map(|m| (m.id.clone(), m.media_type == "video"))
		});
	}
	for selecting in [false, true] {
		let list = if selecting { select_list } else { messages_list };
		let state_looks = state.clone();
		list.set_look_source(move |index, key| {
			let Ok(s) = state_looks.try_borrow() else {
				wxdragon::call_after(Box::new(move || list.refresh(false, None)));
				return None;
			};
			let item = if selecting { s.selection_rows.get(index) } else { s.display_items.get(index) }?;
			if !key.is_empty() && !key.starts_with('\u{1}') && item.id() != key {
				return None;
			}
			let is_group = s.selected_chat.as_deref().is_some_and(|j| j.ends_with("@g.us"));
			Some(crate::ui::message_art::look_for(item, is_group))
		});
	}

	let tx_select = ui_tx.clone();
	parts.conversations_parts.conversations_list.on_selection_changed(move |event| {
		if let Some(idx) = event.get_selection() {
			let _ = tx_select.send(UiCommand::ConversationSelectionChanged(idx as usize));
		}
	});

	let tx_post_status = ui_tx.clone();
	let tx_new_conv = ui_tx.clone();
	let state_new_conv = state.clone();
	parts.conversations_parts.new_conv_btn.on_click(move |_| {
		if state_new_conv.borrow().active_tab == 5 {
			let _ = tx_new_conv.send(UiCommand::FindChannels);
		} else if state_new_conv.borrow().active_tab == 1 {
			let _ = tx_post_status.send(UiCommand::OpenStatusComposer);
		} else {
			let _ = tx_new_conv.send(UiCommand::PrepareContactPicker(crate::ContactPickerMode::OpenChat));
		}
	});

	let tx_post_media_status = ui_tx.clone();
	let state_post_media_status = state.clone();
	parts.conversations_parts.post_media_status_btn.on_click(move |_| {
		if state_post_media_status.borrow().active_tab == 5 {
			let _ = tx_post_media_status.send(UiCommand::CreateChannel);
		} else {
			let _ = tx_post_media_status.send(UiCommand::PostMediaStatus);
		}
	});

	let tx_status_privacy = ui_tx.clone();
	parts.conversations_parts.status_privacy_btn.on_click(move |_| {
		let _ = tx_status_privacy.send(UiCommand::OpenStatusPrivacy);
	});

	let tx_locked_chats = ui_tx.clone();
	let state_locked_chats = state.clone();
	parts.conversations_parts.locked_chats_btn.on_click(move |_| {
		if state_locked_chats.borrow().chats_unlocked {
			let _ = tx_locked_chats.send(UiCommand::CloseLockedChats);
		} else {
			let _ = tx_locked_chats.send(UiCommand::PromptLockedChats);
		}
	});

	let new_conv_dialog_close = parts.new_conv_dialog.dialog;
	parts.new_conv_dialog.close_btn.on_click(move |_| {
		new_conv_dialog_close.end_modal(0);
	});

	let tx_new_contact_form = ui_tx.clone();
	let new_conv_dlg_for_contact = parts.new_conv_dialog.dialog;
	parts.new_conv_dialog.new_contact_btn.on_click(move |_| {
		new_conv_dlg_for_contact.end_modal(0);
		let _ = tx_new_contact_form.send(UiCommand::OpenNewContactForm);
	});

	let new_conv_dlg_for_community = parts.new_conv_dialog.dialog;
	let tx_community = ui_tx.clone();
	parts.new_conv_dialog.new_community_btn.on_click(move |_| {
		new_conv_dlg_for_community.end_modal(0);
		let _ = tx_community.send(UiCommand::NewCommunity);
	});

	let new_conv_dlg_for_meta_ai = parts.new_conv_dialog.dialog;
	let tx_meta_ai = ui_tx.clone();
	parts.new_conv_dialog.message_meta_ai_btn.on_click(move |_| {
		new_conv_dlg_for_meta_ai.end_modal(0);
		let _ = tx_meta_ai.send(UiCommand::OpenChatByJid(crate::types::META_AI_JID.to_string()));
	});

	let tx_picker_filter = ui_tx.clone();
	let picker_search_field = parts.new_conv_dialog.search_field;
	picker_search_field.on_text_changed(move |_| {
		let query = picker_search_field.get_value();
		let _ = tx_picker_filter.send(UiCommand::FilterContactPicker(query));
	});

	// Enter in the search field jumps straight to the top match, so picking
	// a contact never requires touching the results list at all.
	let tx_picker_select_from_search = ui_tx.clone();
	let picker_results_list_from_search = parts.new_conv_dialog.results_list;
	picker_search_field.on_key_down(move |event| {
		if let WindowEventData::Keyboard(ref key_event) = event {
			let code = key_event.get_key_code();
			if keys::is_enter(code) && picker_results_list_from_search.get_count() > 0 {
				let _ = tx_picker_select_from_search.send(UiCommand::SelectContactPickerResult(0));
				event.skip(false);
				return;
			}
			if code == Some(WXK_DOWN) && picker_results_list_from_search.get_count() > 0 {
				picker_results_list_from_search.set_focus();
				if picker_results_list_from_search.get_selection().is_none() {
					picker_results_list_from_search.set_selection(0, true);
				}
				event.skip(false);
				return;
			}
		}
		event.skip(true);
	});

	let tx_picker_select = ui_tx.clone();
	let picker_results_list = parts.new_conv_dialog.results_list;
	picker_results_list.on_item_double_clicked(move |event| {
		if let Some(idx) = event.get_selection() {
			let _ = tx_picker_select.send(UiCommand::SelectContactPickerResult(idx as usize));
		}
	});

	let tx_picker_select_key = ui_tx.clone();
	let picker_results_list_key = parts.new_conv_dialog.results_list;
	let picker_dialog_esc = parts.new_conv_dialog.dialog;
	let picker_search_field_focus = parts.new_conv_dialog.search_field;
	// Plain Enter on a ListBox row never reaches on_key_down here - this
	// dialog's IsDialogMessage handling takes it for the default button (only
	// Alt+Enter got through) - which is what on_enter's key-up half is for.
	keys::on_enter(&picker_results_list_key, Rc::default(), move || {
		if let Some(idx) = picker_results_list_key.get_selection() {
			let _ = tx_picker_select_key.send(UiCommand::SelectContactPickerResult(idx as usize));
		}
	});
	picker_results_list_key.on_key_down(move |event| {
		if let WindowEventData::Keyboard(ref key_event) = event {
			let code = key_event.get_key_code();
			if code == Some(WXK_UP) && picker_results_list_key.get_selection() == Some(0) {
				picker_search_field_focus.set_focus();
				event.skip(false);
				return;
			}
		}
		keys::escape_cancels(picker_dialog_esc)(event);
	});

	let tx_conv_data = ui_tx.clone();
	parts.conversations_parts.conv_data_btn.on_click(move |_| {
		let _ = tx_conv_data.send(UiCommand::ShowConversationData);
	});

	let tx_unblock_banner = ui_tx.clone();
	let state_unblock_banner = state.clone();
	parts.conversations_parts.blocked_banner_btn.on_click(move |_| {
		let Some(jid) = state_unblock_banner.borrow().selected_chat.clone() else { return };
		let _ = tx_unblock_banner.send(UiCommand::ToggleBlockChat { jid });
	});

	let tx_enter_call = ui_tx.clone();
	let tx_place_call = ui_tx.clone();
	parts.conversations_parts.place_call_btn.on_click(move |_| {
		let _ = tx_place_call.send(UiCommand::PlaceCall);
	});
	let tx_place_video_call = ui_tx.clone();
	parts.conversations_parts.place_video_call_btn.on_click(move |_| {
		let _ = tx_place_video_call.send(UiCommand::PlaceVideoCall);
	});
	let tx_join_call = ui_tx.clone();
	parts.conversations_parts.join_call_btn.on_click(move |_| {
		let _ = tx_join_call.send(UiCommand::JoinGroupCall(None));
	});

	let tx_calls_call = ui_tx.clone();
	parts.conversations_parts.calls_call_btn.on_click(move |_| {
		let _ = tx_calls_call.send(UiCommand::PlaceCall);
	});
	let tx_calls_video = ui_tx.clone();
	parts.conversations_parts.calls_video_btn.on_click(move |_| {
		let _ = tx_calls_video.send(UiCommand::PlaceVideoCall);
	});

	let tx_answer = ui_tx.clone();
	parts.call_parts.accept_btn.on_click(move |_| {
		let _ = tx_answer.send(UiCommand::AnswerCall);
	});
	let tx_reject = ui_tx.clone();
	parts.call_parts.reject_btn.on_click(move |_| {
		let _ = tx_reject.send(UiCommand::RejectCall);
	});
	let tx_dismiss = ui_tx.clone();
	parts.call_parts.dismiss_btn.on_click(move |_| {
		let _ = tx_dismiss.send(UiCommand::DismissIncomingCall);
	});
	let tx_callback = ui_tx.clone();
	parts.call_parts.callback_btn.on_click(move |_| {
		let _ = tx_callback.send(UiCommand::RedialCall);
	});
	let tx_close_call = ui_tx.clone();
	parts.call_parts.close_btn.on_click(move |_| {
		let _ = tx_close_call.send(UiCommand::CloseCallWindow);
	});

	let tx_banner_restore = ui_tx.clone();
	parts.conversations_parts.incoming_call_restore_btn.on_click(move |_| {
		let _ = tx_banner_restore.send(UiCommand::RestoreIncomingCall);
	});
	let tx_banner_answer = ui_tx.clone();
	parts.conversations_parts.incoming_call_answer_btn.on_click(move |_| {
		let _ = tx_banner_answer.send(UiCommand::AnswerCall);
	});
	let tx_banner_decline = ui_tx.clone();
	parts.conversations_parts.incoming_call_decline_btn.on_click(move |_| {
		let _ = tx_banner_decline.send(UiCommand::RejectCall);
	});
	let tx_mute = ui_tx.clone();
	parts.call_parts.mute_btn.on_click(move |_| {
		let _ = tx_mute.send(UiCommand::MuteCall);
	});
	let tx_leave = ui_tx.clone();
	parts.call_parts.leave_btn.on_click(move |_| {
		let _ = tx_leave.send(UiCommand::LeaveCall);
	});
	let tx_camera = ui_tx.clone();
	parts.call_parts.camera_btn.on_click(move |_| {
		let _ = tx_camera.send(UiCommand::ToggleCamera);
	});
	let tx_screen = ui_tx.clone();
	parts.call_parts.screen_btn.on_click(move |_| {
		let _ = tx_screen.send(UiCommand::ToggleScreenShare);
	});
	let tx_device_settings = ui_tx.clone();
	parts.call_parts.device_settings_btn.on_click(move |_| {
		let _ = tx_device_settings.send(UiCommand::ToggleCallDeviceSettings);
	});
	let tx_call_input = ui_tx.clone();
	let call_input_combo = parts.call_parts.input_device_combo;
	parts.call_parts.input_device_combo.on_selection_changed(move |_| {
		use crate::ui::dialogs::settings_dialog::device_combo_value;
		let _ = tx_call_input.send(UiCommand::SetCallInputDevice(device_combo_value(&call_input_combo)));
	});
	let tx_call_output = ui_tx.clone();
	let call_output_combo = parts.call_parts.output_device_combo;
	parts.call_parts.output_device_combo.on_selection_changed(move |_| {
		use crate::ui::dialogs::settings_dialog::device_combo_value;
		let _ = tx_call_output.send(UiCommand::SetCallOutputDevice(device_combo_value(&call_output_combo)));
	});
	let tx_add_participant = ui_tx.clone();
	parts.call_parts.add_participant_btn.on_click(move |_| {
		let _ = tx_add_participant.send(UiCommand::PrepareContactPicker(crate::ContactPickerMode::AddCallParticipant));
	});
	let tx_raise_hand = ui_tx.clone();
	parts.call_parts.raise_hand_btn.on_click(move |_| {
		let _ = tx_raise_hand.send(UiCommand::ToggleCallHand);
	});
	let tx_call_react = ui_tx.clone();
	parts.call_parts.react_btn.on_click(move |_| {
		let _ = tx_call_react.send(UiCommand::CallReact);
	});
	let tx_person_selected = ui_tx.clone();
	parts.call_parts.people_list.on_selection_changed(move |_| {
		let _ = tx_person_selected.send(UiCommand::CallPersonSelected);
	});
	let tx_mute_person = ui_tx.clone();
	parts.call_parts.mute_person_btn.on_click(move |_| {
		let _ = tx_mute_person.send(UiCommand::MuteCallPerson);
	});
	let tx_remove_person = ui_tx.clone();
	parts.call_parts.remove_person_btn.on_click(move |_| {
		let _ = tx_remove_person.send(UiCommand::RemoveCallPerson);
	});
	let tx_let_in = ui_tx.clone();
	parts.call_parts.let_in_btn.on_click(move |_| {
		let _ = tx_let_in.send(UiCommand::AdmitCallPerson(true));
	});
	let tx_keep_out = ui_tx.clone();
	parts.call_parts.keep_out_btn.on_click(move |_| {
		let _ = tx_keep_out.send(UiCommand::AdmitCallPerson(false));
	});
	let tx_approval = ui_tx.clone();
	parts.call_parts.approval_check.on_toggled(move |event| {
		let _ = tx_approval.send(UiCommand::SetCallApproval(event.is_checked()));
	});
	let tx_call_camera = ui_tx.clone();
	let call_camera_combo = parts.call_parts.camera_device_combo;
	parts.call_parts.camera_device_combo.on_selection_changed(move |_| {
		use crate::ui::dialogs::settings_dialog::device_combo_value;
		let _ = tx_call_camera.send(UiCommand::SetCallCameraDevice(device_combo_value(&call_camera_combo)));
	});
	// Closing the call window (Alt+F4, its close box) must never destroy it:
	// it's built once and reused for every call, so a destroyed one left
	// every later call - incoming or outgoing - with no window at all until
	// the app restarted. Closing hides it and does what the button on
	// screen would: decline a call that's ringing, otherwise hang up.
	let tx_call_close = ui_tx.clone();
	let call_frame_close = parts.call_parts.frame;
	let state_call_close = state.clone();
	parts.call_parts.frame.on_close(move |event| {
		if let WindowEventData::General(ref ev) = event {
			if !ev.can_veto() {
				return;
			}
			ev.veto();
			call_frame_close.show(false);
			let s = state_call_close.borrow();
			let ringing_in = s.incoming_call_caller.is_some() && s.active_call_audio.is_none();
			let in_call = s.active_call_id.is_some() || s.call_placing;
			drop(s);
			if ringing_in {
				let _ = tx_call_close.send(UiCommand::DismissIncomingCall);
			} else if in_call {
				let _ = tx_call_close.send(UiCommand::LeaveCall);
			}
		}
	});

	let call_duration_timer = Box::leak(Box::new(Timer::new(&parts.call_parts.frame)));
	let state_timer = state.clone();
	let call_status_label = parts.call_parts.status_label;
	let call_caller_info = parts.call_parts.caller_info;
	let call_frame_timer = parts.call_parts.frame;
	call_duration_timer.on_tick(move |_| {
		let s = state_timer.borrow();
		if s.active_call_id.is_some() && s.active_call_audio.is_some() {
			let elapsed =
				s.call_connected_duration + s.call_connected_segment_start.map(|t| t.elapsed()).unwrap_or_default();
			let secs = elapsed.as_secs();
			let clock = crate::calls::helpers::format_call_duration(secs);
			let is_saved = s.call_peer_is_saved;
			let is_reconnecting = s.call_is_reconnecting;
			let peer_name = s.call_peer_name.clone();
			let is_incoming = s.call_shows_incoming();
			let is_video = s.call_video.video_call || s.call_video.is_video || s.call_video.local_camera_on;
			let is_group = s.call_participants.len() > 1 || s.call_video.peer_name.is_none();
			drop(s);

			// The heading never carries the clock: the duration lives in the
			// call info box, so the heading reads the same for the whole call.
			let status = if is_reconnecting {
				"Reconnecting...".to_string()
			} else if is_group {
				"In group call".to_string()
			} else {
				"Connected".to_string()
			};
			call_status_label.set_label(&status);

			let running_info =
				crate::calls::helpers::call_running_info_text(is_incoming, is_video, &peer_name, secs, is_reconnecting);
			// Rewriting the read-only box every second would reset a screen
			// reader's caret while the user reads it, so refresh it only every
			// 30 seconds and keep the caret where it was.
			if secs % 30 == 0 && call_caller_info.get_value() != running_info {
				let caret = call_caller_info.get_insertion_point();
				call_caller_info.set_value(&running_info);
				call_caller_info.set_insertion_point(caret);
			}

			let win_title = crate::calls::helpers::format_call_window_title(
				is_incoming,
				is_group,
				is_video,
				&peer_name,
				is_saved,
				Some(&clock),
			);
			call_frame_timer.set_title(&win_title);
		}
	});
	call_duration_timer.start(1000, false);

	let tx_camera_key = ui_tx.clone();
	let state_call_key = state.clone();
	parts.call_parts.frame.on_key_down(move |event| {
		if let WindowEventData::Keyboard(ref key_event) = event {
			let key_code = key_event.get_key_code();
			if key_event.control_down() && key_event.shift_down() {
				let command = if keys::is_letter(key_code, 'V') {
					Some(UiCommand::ToggleCamera)
				} else if keys::is_letter(key_code, 'S') {
					Some(UiCommand::ToggleScreenShare)
				} else if keys::is_letter(key_code, 'M') {
					Some(UiCommand::MuteCall)
				} else if keys::is_letter(key_code, 'D') {
					Some(UiCommand::ToggleCallDeviceSettings)
				} else {
					None
				};
				if let Some(command) = command {
					let _ = tx_camera_key.send(command);
					event.skip(false);
					return;
				}
			} else if key_code == Some(27) {
				let s = state_call_key.borrow();
				let ringing_in = s.incoming_call_caller.is_some() && s.active_call_audio.is_none();
				let call_active = s.active_call_id.is_some() || s.call_placing;
				drop(s);
				if ringing_in {
					let _ = tx_camera_key.send(UiCommand::DismissIncomingCall);
					event.skip(false);
					return;
				} else if !call_active {
					let _ = tx_camera_key.send(UiCommand::CloseCallWindow);
					event.skip(false);
					return;
				}
			}
		}
		event.skip(true);
	});

	for (button, video) in
		[(parts.conversation_data_dialog.voice_call_btn, false), (parts.conversation_data_dialog.video_call_btn, true)]
	{
		let tx_call = ui_tx.clone();
		let state_call = state.clone();
		let info_dialog = parts.conversation_data_dialog.dialog;
		let details_dialog = parts.conversation_data_dialog.details_dialog;
		button.on_click(move |_| {
			let Some(jid) = state_call.borrow().info_chat.clone() else { return };
			details_dialog.end_modal(wxdragon::id::ID_CANCEL);
			info_dialog.end_modal(wxdragon::id::ID_CANCEL);
			let _ = tx_call.send(UiCommand::CallContact { jid, video });
		});
	}

	let tx_contact = ui_tx.clone();
	parts.conversation_data_dialog.contact_btn.on_click(move |_| {
		let _ = tx_contact.send(UiCommand::AddOrEditContact);
	});
	let tx_delete_contact = ui_tx.clone();
	parts.conversation_data_dialog.delete_contact_btn.on_click(move |_| {
		let _ = tx_delete_contact.send(UiCommand::DeleteContact);
	});
	let tx_info_privacy = ui_tx.clone();
	parts.conversation_data_dialog.chat_privacy_btn.on_click(move |_| {
		let _ = tx_info_privacy.send(UiCommand::ShowChatPrivacy);
	});

	crate::ui::chat_overview::wire(&parts, &state, &ui_tx);

	let conv_data_dialog_back = parts.conversation_data_dialog.dialog;
	parts.conversation_data_dialog.back_btn.on_click(move |_| {
		conv_data_dialog_back.end_modal(0);
	});

	let group_data_dialog_back = parts.group_data_dialog.dialog;
	parts.group_data_dialog.back_btn.on_click(move |_| {
		group_data_dialog_back.end_modal(0);
	});

	let tray_icon = parts.tray_icon.clone();
	let tray_bitmap = theme::build_app_icon(32);
	// Whether an Enter release still to come has already been dealt with, so
	// the chat list mustn't open a chat on it. See its uses on the chat list.
	let enter_release_handled = crate::platform::tray::enter_release_guard();

	let tray_open_id = 5001;
	let tray_exit_id = 5002;
	let mut tray_menu = Menu::builder()
		.append_item(tray_open_id, "&Open whatSnap", "Show the main window")
		.append_item(tray_exit_id, "E&xit", "Quit whatSnap")
		.build();
	let frame_tray_menu = parts.frame;
	let tx_tray_menu = ui_tx.clone();
	tray_icon.on_menu(move |event| {
		let id = event.get_id();
		if id == tray_open_id {
			crate::platform::tray::restore_main_window(&frame_tray_menu);
			let _ = tx_tray_menu.send(UiCommand::SetPresence(true));
		} else if id == tray_exit_id {
			// Like Fedra: the menu handler only asks for Exit. The icon is
			// still inside its popup-menu call here, so it mustn't be taken
			// out of the notification area until that returns.
			let _ = tx_tray_menu.send(UiCommand::ExitApp);
		}
	});
	tray_icon.set_popup_menu(&mut tray_menu);
	std::mem::forget(tray_menu);

	let frame_tray_select = parts.frame;
	let tx_tray_select = ui_tx.clone();
	crate::platform::tray::set_on_select(move || {
		crate::platform::tray::restore_main_window(&frame_tray_select);
		let _ = tx_tray_select.send(UiCommand::SetPresence(true));
	});

	let apply_tray_setting = Rc::new({
		let tray_icon = tray_icon.clone();
		let tray_bitmap = tray_bitmap.clone();
		let state = state.clone();
		move |enabled: bool| {
			if crate::platform::quit::is_quitting() {
				return;
			}
			if enabled {
				let s = state.borrow();
				tray_icon.set_icon(&tray_bitmap, whatsnap::TRAY_APP_NAME);
				let unread = whatsnap::unread_notification_total(
					&s.unread_counts,
					&s.settings.quiet_chats(&s.muted_jids),
					&s.archived_jids,
					&s.settings.archived_alert_levels,
				);
				tray_icon.set_icon(&tray_bitmap, &crate::ui_commands::accounts::tray_text(&s, unread));
				crate::platform::tray::enable_keyboard_select();
			} else {
				tray_icon.remove_icon();
			}
		}
	});
	{
		let apply_tray_setting = apply_tray_setting.clone();
		let tray_icon = tray_icon.clone();
		let state = state.clone();
		crate::platform::tray::set_on_taskbar_created(move || {
			let Ok(s) = state.try_borrow() else {
				return false;
			};
			let shown = s.settings.show_tray_icon && s.main_ui_shown;
			drop(s);
			if shown {
				tray_icon.remove_icon();
				apply_tray_setting(true);
			}
			true
		});
	}
	// Only ever install the tray icon while logged in - there's nothing to
	// keep running in the background for otherwise, and a tray icon for an
	// unpaired app is just clutter (and one more stray thing for a screen
	// reader to trip over). main_ui_shown is false until "connected" lands, so
	// this installs nothing at startup; the connected/needs_login handlers in
	// main.rs reconcile it on every login state change thereafter.
	let show_tray_now = {
		let s = state.borrow();
		s.settings.show_tray_icon && s.main_ui_shown
	};
	apply_tray_setting(show_tray_now);

	let frame_close = parts.frame;
	let tray_icon_close = tray_icon.clone();
	let tx_close = ui_tx.clone();
	let state_close = state.clone();
	let call_frame_quit = parts.call_parts.frame;
	let frame_login_close = parts.frame;
	let call_frame_login_close = parts.call_parts.frame;
	parts.frame.on_close(move |event| {
		if let WindowEventData::General(ref ev) = event {
			// Only hide-to-tray while logged in - a signed-in session is worth
			// keeping alive in the background to receive messages/calls. When
			// we're not logged in there's nothing to stay running for, so
			// Alt+F4 should actually quit instead of parking a stray (and, to a
			// screen reader, oddly-announced) window in the tray. main_ui_shown
			// is true only once "connected" has shown the chat list.
			let logged_in = state_close.borrow().main_ui_shown;
			if logged_in && ev.can_veto() && tray_icon_close.is_icon_installed() {
				ev.veto();
				crate::platform::tray::hide_main_window(&frame_close);
				let _ = tx_close.send(UiCommand::WindowHidden);
			} else {
				crate::platform::tray::quit_app(&tray_icon_close, &[frame_close, call_frame_quit]);
			}
		}
	});

	let tray_icon_login_close = tray_icon.clone();
	let login_panel_close = parts.login_panel;
	let state_login_close = state.clone();
	let tx_login_close = ui_tx.clone();
	parts.login_panel.on_close(move |event| {
		if let WindowEventData::General(_) = event {
			// With another account linked, closing the login window goes back
			// to it instead of quitting.
			if crate::ui_commands::accounts::has_other_account(&state_login_close) {
				let _ = tx_login_close.send(UiCommand::LeaveLogin);
				return;
			}
			login_panel_close.show(false);
			crate::platform::tray::quit_app(&tray_icon_login_close, &[frame_login_close, call_frame_login_close]);
		}
	});

	let tx_activate = ui_tx.clone();
	parts.frame.on_activate(move |event| {
		if let WindowEventData::Activate(ref ev) = event {
			if ev.is_active() && crate::platform::tray::is_hidden_to_tray() {
				return;
			}
			if ev.is_active() && enter_key_held() {
				crate::platform::tray::enter_release_guard().set(true);
			}
			let _ = tx_activate.send(UiCommand::SetPresence(ev.is_active()));
		}
	});

	let keymap = Rc::new(crate::keymap::Keymap::from_saved(&state.borrow().settings.shortcuts));
	keymap.apply(&parts.frame);
	state.borrow_mut().keymap = keymap;
	let pending_shortcuts: Rc<std::cell::RefCell<Option<crate::keymap::Keymap>>> = Rc::default();

	let settings_dialog = parts.settings_dialog.dialog;
	let sounds_check = parts.settings_dialog.sounds_check;
	let notifications_check = parts.settings_dialog.notifications_check;
	let follow_dnd_check_open = parts.settings_dialog.follow_dnd_check;
	let announce_typing_check = parts.settings_dialog.announce_typing_check;
	let announce_recording_check = parts.settings_dialog.announce_recording_check;
	let tray_icon_check = parts.settings_dialog.tray_icon_check;
	let autostart_check = parts.settings_dialog.autostart_check;
	let start_maximized_check_open = parts.settings_dialog.start_maximized_check;
	let check_updates_check_open = parts.settings_dialog.check_updates_check;
	let update_channel_combo_open = parts.settings_dialog.update_channel_combo;
	let meta_ai_enabled_check_open = parts.settings_dialog.meta_ai_enabled_check;
	let verbose_logging_check_open = parts.settings_dialog.verbose_backend_logging_check;
	let name_field_open = parts.settings_dialog.name_field;
	let settings_parts_open = parts.settings_dialog;
	let input_device_combo_open = parts.settings_dialog.input_device_combo;
	let output_device_combo_open = parts.settings_dialog.output_device_combo;
	let call_output_device_combo_open = parts.settings_dialog.call_output_device_combo;
	let noise_suppression_check_open = parts.settings_dialog.voice_message_noise_suppression_check;
	let dual_stream_check_open = parts.settings_dialog.dual_stream_screen_share_check;
	let state_settings_open = state.clone();
	let pending_shortcuts_open = pending_shortcuts.clone();
	// Settings can be asked for while something further up is still using
	// the app state (a dialog or menu opened from a call, say). Opening it
	// then would panic and nothing would show, so it tries again shortly.
	let settings_retry = Rc::new(Timer::new(&parts.frame));
	let settings_retries = Rc::new(std::cell::Cell::new(0u32));
	let frame_settings_retry = parts.frame;
	settings_retry.on_tick(move |_| unsafe {
		wxdragon::ffi::wxd_Window_PostMenuCommand(
			frame_settings_retry.handle_ptr() as *mut wxdragon::ffi::wxd_Window_t,
			MENU_ID_SETTINGS,
		);
	});
	let settings_retry_open = settings_retry.clone();
	parts.frame.on_menu_selected(move |evt| {
		if evt.get_id() != MENU_ID_SETTINGS {
			return;
		}
		if settings_dialog.is_shown() {
			settings_dialog.raise();
			return;
		}
		let Ok(mut s) = state_settings_open.try_borrow_mut() else {
			if settings_retries.get() < 40 {
				settings_retries.set(settings_retries.get() + 1);
				settings_retry_open.start(250, true);
			}
			return;
		};
		settings_retries.set(0);
		pending_shortcuts_open.borrow_mut().take();
		use crate::ui::dialogs::settings_dialog::*;
		sounds_check.set_value(s.settings.sounds_enabled);
		notifications_check.set_value(s.settings.notifications_enabled);
		follow_dnd_check_open.set_value(s.settings.follow_windows_do_not_disturb);
		announce_typing_check.set_value(s.settings.announce_typing);
		announce_recording_check.set_value(s.settings.announce_recording);
		tray_icon_check.set_value(s.settings.show_tray_icon);
		meta_ai_enabled_check_open.set_value(s.settings.meta_ai_enabled);
		verbose_logging_check_open.set_value(s.settings.verbose_backend_logging);
		start_maximized_check_open.set_value(s.settings.start_maximized);
		check_updates_check_open.set_value(s.settings.check_for_updates_on_startup);
		crate::ui::dialogs::settings_dialog::set_combo_from_value(
			&update_channel_combo_open,
			crate::ui::dialogs::settings_dialog::UPDATE_CHANNEL_CHOICES,
			&s.settings.update_channel,
		);
		noise_suppression_check_open.set_value(s.settings.voice_message_noise_suppression);
		dual_stream_check_open.set_value(s.settings.dual_stream_screen_share);

		name_field_open.set_value(&s.own_push_name);
		s.own_about_loaded = false;
		s.default_timer = None;
		settings_parts_open.default_timer_combo.enable(false);
		s.privacy_settings_loaded = false;
		s.privacy_lists.clear();
		s.privacy_list_edits.clear();
		settings_parts_open.show_about(None);
		settings_parts_open.show_privacy(None);
		s.app_prefs = None;
		settings_parts_open.show_app_prefs(None);
		settings_parts_open.show_chat_settings(&s.settings);
		settings_parts_open.match_theme();

		set_device_combo(
			&input_device_combo_open,
			&crate::audio::devices::input_device_names(),
			s.settings.input_device_name.as_deref(),
		);
		set_device_combo(
			&output_device_combo_open,
			&crate::audio::devices::output_device_names(),
			s.settings.output_device_name.as_deref(),
		);
		set_device_combo(
			&call_output_device_combo_open,
			&crate::audio::devices::output_device_names(),
			s.settings.call_output_device_name.as_deref(),
		);

		let _ = s.ws_tx.send(serde_json::json!({"action": "get_own_profile"}).to_string());
		let _ = s.ws_tx.send(serde_json::json!({"action": "get_default_timer"}).to_string());
		let _ = s.ws_tx.send(serde_json::json!({"action": "get_privacy_settings"}).to_string());
		let _ = s.ws_tx.send(serde_json::json!({"action": "get_app_prefs"}).to_string());
		for control in settings_parts_open.privacy_controls() {
			if control.except_btn.is_some() {
				let payload = serde_json::json!({"action": "get_privacy_list", "data": {"name": control.name}});
				let _ = s.ws_tx.send(payload.to_string());
			}
		}
		let _ = s.ws_tx.send(serde_json::json!({"action": "get_contacts"}).to_string());
		drop(s);
		autostart_check.set_value(crate::platform::autostart::is_enabled());
		crate::ui::show_modal_again(&settings_dialog);
	});

	let frame_logout = parts.frame;
	let state_logout = state.clone();
	parts.frame.on_menu_selected(move |evt| {
		if evt.get_id() == MENU_ID_LOGOUT {
			confirm_logout(&frame_logout, &state_logout);
		}
	});

	let frame_updates = parts.frame;
	let state_updates = state.clone();
	let tx_updates = ui_tx.clone();
	parts.frame.on_menu_selected(move |evt| {
		if evt.get_id() != MENU_ID_CHECK_UPDATES {
			return;
		}
		let channel = state_updates.borrow().settings.update_channel.clone();
		crate::platform::update_check::run(&frame_updates, &tx_updates, &channel, false);
	});

	let tx_accounts = ui_tx.clone();
	parts.frame.on_menu_selected(move |evt| {
		if evt.get_id() == MENU_ID_ACCOUNTS {
			let _ = tx_accounts.send(UiCommand::OpenAccounts(None));
		}
	});

	let state_reconnect = state.clone();
	let reconnect_announcer = parts.conversations_parts.messages_announcer;
	parts.frame.on_menu_selected(move |evt| {
		if evt.get_id() != MENU_ID_RECONNECT {
			return;
		}
		let mut s = state_reconnect.borrow_mut();
		s.conn_manual_reconnect = true;
		let online = s.conn_online;
		let _ = s.ws_tx.send(serde_json::json!({"action": "reconnect"}).to_string());
		drop(s);
		if !online {
			crate::platform::announce::announce(reconnect_announcer, "Reconnecting...");
		}
	});

	let nav_list_for_menu = parts.nav_list;
	let state_nav_menu = state.clone();
	let nav_announcer = parts.conversations_parts.messages_announcer;
	let tx_nav_menu = ui_tx.clone();
	let nav_focus_timer = std::rc::Rc::new(Timer::new(&parts.frame));
	let conv_list_for_nav_timer = parts.conversations_parts.conversations_list;
	nav_focus_timer.on_tick(move |_| {
		conv_list_for_nav_timer.set_focus();
		if conv_list_for_nav_timer.get_selection().is_none() && conv_list_for_nav_timer.get_count() > 0 {
			conv_list_for_nav_timer.set_selection(0_u32, true);
		}
	});
	let return_pane = std::rc::Rc::new(std::cell::Cell::new(ReturnPane::Messages));
	let return_spot: std::rc::Rc<std::cell::RefCell<Option<ReturnSpot>>> =
		std::rc::Rc::new(std::cell::RefCell::new(None));
	let rp_nav = return_pane.clone();
	parts.nav_list.on_set_focus(move |event| {
		rp_nav.set(ReturnPane::Nav);
		event.skip(true);
	});
	let rp_chats = return_pane.clone();
	parts.conversations_parts.conversations_list.on_set_focus(move |event| {
		rp_chats.set(ReturnPane::Chats);
		event.skip(true);
	});
	let conv_list_focus_select = parts.conversations_parts.conversations_list;
	parts.conversations_parts.conversations_list.on_set_focus(move |event| {
		if conv_list_focus_select.get_selection().is_none() && conv_list_focus_select.get_count() > 0 {
			conv_list_focus_select.set_selection(0_u32, true);
		}
		event.skip(true);
	});
	let rp_messages = return_pane.clone();
	parts.conversations_parts.messages_list.on_set_focus(move |event| {
		rp_messages.set(ReturnPane::Messages);
		event.skip(true);
	});
	let spot_on_leave = return_spot.clone();
	parts.conversations_parts.message_field.on_kill_focus(move |event| {
		spot_on_leave.borrow_mut().take();
		event.skip(true);
	});
	parts.frame.on_menu_selected(move |evt| {
		let tab = match evt.get_id() {
			MENU_ID_NAV_CONVERSATIONS => 0,
			MENU_ID_NAV_STATUS => 1,
			MENU_ID_NAV_STARRED => 2,
			MENU_ID_NAV_ARCHIVED => 3,
			MENU_ID_NAV_COMMUNITIES => 4,
			MENU_ID_NAV_CHANNELS => 5,
			MENU_ID_NAV_CALLS => 6,
			_ => return,
		};
		let idx = state_nav_menu.borrow().settings.tab_order().iter().position(|&t| t == tab).unwrap_or(tab) as u32;
		// set_selection's bool means select-vs-deselect, not "fire the change
		// event" - wxListBox::SetSelection never generates one, so switching
		// the highlighted tab needs an explicit NavSelected to actually swap
		// the (shared) list's content, same as a mouse click would trigger.
		nav_list_for_menu.set_selection(idx, true);
		let _ = tx_nav_menu.send(UiCommand::NavSelected(tab));
		let name = nav_list_for_menu.get_string(idx as u32).unwrap_or_default();
		if !name.is_empty() {
			crate::platform::announce::announce(nav_announcer, &name);
		}
		nav_focus_timer.start(700, true);
	});

	let state_chat_menu = state.clone();
	let tx_chat_menu = ui_tx.clone();
	let messages_list_for_menu = parts.conversations_parts.messages_list;
	let search_field_for_menu = parts.conversations_parts.search_field;
	let msg_field_for_menu = parts.conversations_parts.message_field;
	let msgs_announcer_for_menu = parts.conversations_parts.messages_announcer;
	let nav_list_for_focus_menu = parts.nav_list;
	let conv_list_for_focus_menu = parts.conversations_parts.conversations_list;
	let return_pane_for_menu = return_pane.clone();
	let return_spot_for_menu = return_spot.clone();
	parts.frame.on_menu_selected(move |evt| {
		let id = evt.get_id();
		// Belt and braces for the greying out: never act on a chat that
		// isn't the one being looked at.
		let list_chat = CHAT_LIST_ACTIONS
			.iter()
			.any(|(action, _)| *action == id)
			.then(|| chat_list_focus_chat(&state_chat_menu.borrow(), &conv_list_for_focus_menu))
			.flatten();
		if CONVERSATION_MENU_IDS.contains(&id)
			&& list_chat.is_none()
			&& conversation_menu_chat(&state_chat_menu.borrow()).is_none()
		{
			return;
		}
		if id == MENU_ID_NEW_CHAT {
			let _ = tx_chat_menu.send(UiCommand::PrepareContactPicker(crate::ContactPickerMode::OpenChat));
		} else if id == MENU_ID_NEW_GROUP {
			let _ = tx_chat_menu.send(UiCommand::PrepareNewGroup);
		} else if id == MENU_ID_NEW_COMMUNITY {
			let _ = tx_chat_menu.send(UiCommand::NewCommunity);
		} else if id == MENU_ID_MANAGE_LISTS {
			let _ = tx_chat_menu.send(UiCommand::ManageChatLists);
		} else if id == MENU_ID_SEARCH_GLOBAL {
			search_field_for_menu.set_focus();
		} else if id == MENU_ID_SEARCH_CHAT {
			let _ = tx_chat_menu.send(UiCommand::ToggleSearchPanel);
		} else if id == MENU_ID_FOCUS_MESSAGE_FIELD {
			if !msg_field_for_menu.is_shown() {
				crate::platform::announce::announce(msgs_announcer_for_menu, "This chat has no message box.");
			} else if msg_field_for_menu.has_focus() {
				let spot = return_spot_for_menu.borrow_mut().take();
				let chat = state_chat_menu.borrow().selected_chat.clone();
				let restored = match &spot {
					Some(ReturnSpot::Control(handle)) => restore_focus(*handle),
					_ => false,
				};
				if !restored {
					let (pane, index) = match spot {
						Some(ReturnSpot::Pane { pane, index, chat: spot_chat }) => {
							let index =
								if matches!(pane, ReturnPane::Messages) && spot_chat != chat { None } else { index };
							(pane, index)
						}
						_ => (return_pane_for_menu.get(), None),
					};
					let list: &dyn CursorList = match pane {
						ReturnPane::Nav => &nav_list_for_focus_menu,
						ReturnPane::Chats => &conv_list_for_focus_menu,
						ReturnPane::Messages => &messages_list_for_menu,
					};
					if let Some(index) = index.filter(|&i| i < list.count()) {
						list.select_and_show(index);
					}
					list.focus();
				}
			} else {
				let chat = state_chat_menu.borrow().selected_chat.clone();
				let focused = unsafe { windows::Win32::UI::Input::KeyboardAndMouse::GetFocus() }.0 as usize;
				let panes: [(ReturnPane, &dyn CursorList); 3] = [
					(ReturnPane::Nav, &nav_list_for_focus_menu),
					(ReturnPane::Chats, &conv_list_for_focus_menu),
					(ReturnPane::Messages, &messages_list_for_menu),
				];
				let spot = if let Some((pane, list)) = panes.iter().find(|(_, list)| list.hwnd() == focused) {
					ReturnSpot::Pane { pane: *pane, index: list.selection(), chat }
				} else if focused != 0 {
					ReturnSpot::Control(focused)
				} else {
					let pane = return_pane_for_menu.get();
					let list: &dyn CursorList = match pane {
						ReturnPane::Nav => &nav_list_for_focus_menu,
						ReturnPane::Chats => &conv_list_for_focus_menu,
						ReturnPane::Messages => &messages_list_for_menu,
					};
					ReturnSpot::Pane { pane, index: list.selection(), chat }
				};
				*return_spot_for_menu.borrow_mut() = Some(spot);
				msg_field_for_menu.set_focus();
			}
		} else if id == MENU_ID_FOCUS_MESSAGES_LIST {
			if messages_list_for_menu.is_shown() {
				messages_list_for_menu.set_focus();
			}
		} else if id == MENU_ID_ANNOUNCE_STATUS {
			let label = {
				let s = state_chat_menu.borrow();
				s.selected_chat.clone().map(|jid| crate::ui::chat_ui::chat_status_label(&s, &jid))
			};
			if let Some(label) = label {
				crate::platform::announce::announce(msgs_announcer_for_menu, &label);
			}
		} else if id == MENU_ID_TOGGLE_ANNOUNCE {
			let said = {
				let mut s = state_chat_menu.borrow_mut();
				s.selected_chat.clone().map(|jid| {
					let on = s.settings.toggle_chat_announce(&jid);
					s.settings.save();
					if on {
						"New messages in this chat will be read aloud."
					} else {
						"New messages in this chat won't be read aloud."
					}
				})
			};
			if let Some(said) = said {
				crate::platform::announce::announce(msgs_announcer_for_menu, said);
			}
		} else if id == MENU_ID_NEXT_SPELLING_ERROR {
			if !msg_field_for_menu.is_shown() {
				crate::platform::announce::announce(msgs_announcer_for_menu, "This chat has no message box.");
			} else if let Some(say) = crate::ui::spell_check::next_error(&msg_field_for_menu) {
				crate::platform::announce::announce(msgs_announcer_for_menu, say);
			}
		} else if id == MENU_ID_EMOJI_PANEL || id == MENU_ID_STICKER_PANEL {
			let _ = tx_chat_menu.send(UiCommand::OpenEmojiPanel { stickers: id == MENU_ID_STICKER_PANEL });
		} else if id == MENU_ID_CHAT_INFO {
			let _ = tx_chat_menu.send(UiCommand::ShowConversationData);
		} else if id == MENU_ID_DISAPPEARING {
			let jid = state_chat_menu.borrow().selected_chat.clone();
			if let Some(jid) = jid {
				let _ = tx_chat_menu.send(UiCommand::ChooseDisappearingTimer { jid });
			}
		} else if id == MENU_ID_GO_TO_DATE {
			let _ = tx_chat_menu.send(UiCommand::GoToDate);
		} else if id == MENU_ID_SCHEDULE_MESSAGE {
			let _ = tx_chat_menu.send(UiCommand::OpenScheduleDialog);
		} else if id == MENU_ID_SCHEDULED_MESSAGES {
			let _ = tx_chat_menu.send(UiCommand::ShowScheduledMessages);
		} else if id == MENU_ID_REPORT_CHAT {
			let jid = state_chat_menu.borrow().selected_chat.clone();
			if let Some(jid) = jid {
				let _ = tx_chat_menu.send(UiCommand::ReportChat { jid });
			}
		} else if id == MENU_ID_BLOCK_CHAT {
			let jid = state_chat_menu.borrow().selected_chat.clone();
			if let Some(jid) = jid {
				let _ = tx_chat_menu.send(UiCommand::ToggleBlockChat { jid });
			}
		} else if id == MENU_ID_BUSINESS_CATALOG {
			let _ = tx_chat_menu.send(UiCommand::OpenCatalog(None));
		} else if id == MENU_ID_BLOCKED_CONTACTS {
			let _ = tx_chat_menu.send(UiCommand::ShowBlockedContacts);
		} else if id == MENU_ID_PINNED_MESSAGES {
			crate::ui_commands::message_extras::show_pinned_messages(&state_chat_menu);
		} else if id == MENU_ID_CHAT_PRIVACY {
			crate::ui_commands::message_extras::show_chat_privacy(&state_chat_menu);
		} else if id == MENU_ID_MUTE_CHAT
			|| id == MENU_ID_ARCHIVE_CHAT
			|| id == MENU_ID_PIN_CHAT
			|| id == MENU_ID_LOCK_CHAT
			|| id == MENU_ID_MARK_UNREAD
		{
			let s = state_chat_menu.borrow();
			let Some(jid) = list_chat.or_else(|| s.selected_chat.clone()) else { return };
			if id == MENU_ID_MUTE_CHAT {
				let muted = s.muted_jids.contains(&jid);
				drop(s);
				let _ = tx_chat_menu.send(if muted {
					UiCommand::ToggleMuteChat { jid, mute: false, seconds: 0 }
				} else {
					UiCommand::ChooseMuteDuration { jid }
				});
			} else if id == MENU_ID_ARCHIVE_CHAT {
				let archive = !s.archived_jids.contains(&jid);
				drop(s);
				let _ = tx_chat_menu.send(UiCommand::ToggleArchiveChat { jid, archive });
			} else if id == MENU_ID_PIN_CHAT {
				let pin = !s.pinned_jids.contains(&jid);
				drop(s);
				let _ = tx_chat_menu.send(UiCommand::TogglePinChat { jid, pin });
			} else if id == MENU_ID_LOCK_CHAT {
				let lock = !s.locked_jids.contains(&jid);
				drop(s);
				let _ = tx_chat_menu.send(UiCommand::ToggleLockChat { jid, lock });
			} else {
				let read = s.unread_counts.get(&jid).copied().unwrap_or(0) == 0;
				drop(s);
				let _ = tx_chat_menu.send(UiCommand::ToggleReadChat { jid, read: !read });
			}
		} else if id == MENU_ID_STAR_MESSAGE
			&& let Some(idx) = messages_list_for_menu.get_selection()
		{
			let s = state_chat_menu.borrow();
			let jid = s.selected_chat.clone().unwrap_or_default();
			let item_opt = s.display_items.get(idx as usize).cloned();
			drop(s);
			if let Some(item) = item_opt {
				match item {
					whatsnap::DisplayItem::Single(msg) => {
						if !msg.is_date_divider && crate::ui_commands::selection::can_star(&jid, &msg) {
							let sender_jid = if msg.from_me { "me".to_string() } else { msg.sender_jid.clone() };
							let _ = tx_chat_menu.send(UiCommand::StarMessage {
								message_id: msg.id.clone(),
								sender_jid,
								from_me: msg.from_me,
								starred: !msg.starred,
							});
						}
					}
					whatsnap::DisplayItem::Album { messages, .. } => {
						let all_starred = !messages.is_empty() && messages.iter().all(|m| m.starred);
						let target_starred = !all_starred;
						for m in messages.iter().filter(|m| {
							m.starred != target_starred && crate::ui_commands::selection::can_star(&jid, m)
						}) {
							let sender_jid = if m.from_me { "me".to_string() } else { m.sender_jid.clone() };
							let _ = tx_chat_menu.send(UiCommand::StarMessage {
								message_id: m.id.clone(),
								sender_jid,
								from_me: m.from_me,
								starred: target_starred,
							});
						}
					}
				}
			}
		}
	});

	let tx_voice_keys = ui_tx.clone();
	let state_voice_keys = state.clone();
	parts.frame.on_char_hook(move |event| {
		if let WindowEventData::Keyboard(ref key_event) = event {
			let keymap = state_voice_keys.borrow().keymap.clone();
			let command = if keymap.is_pressed(voice::START_RECORDING, key_event) {
				Some(UiCommand::StartRecording)
			} else if keymap.is_pressed(voice::PAUSE_RECORDING, key_event) {
				Some(UiCommand::TogglePauseRecording)
			} else if keymap.is_pressed(voice::TOGGLE_PREVIEW, key_event) {
				Some(UiCommand::ToggleVoicePreview)
			} else if keymap.is_pressed(voice::DISCARD, key_event) {
				Some(UiCommand::DiscardVoiceMessage)
			} else if keymap.is_pressed(voice::SEND, key_event) {
				Some(UiCommand::SendVoiceMessage)
			} else {
				None
			};
			match command {
				Some(command) => {
					let _ = tx_voice_keys.send(command);
					event.skip(false);
				}
				None => event.skip(true),
			}
		}
	});

	let tx_call_menu = ui_tx.clone();
	parts.frame.on_menu_selected(move |evt| {
		let id = evt.get_id();
		if id == MENU_ID_CALL_MUTE {
			let _ = tx_call_menu.send(UiCommand::MuteCall);
		} else if id == MENU_ID_CALL_END {
			let _ = tx_call_menu.send(UiCommand::LeaveCall);
		} else if id == MENU_ID_CREATE_CALL_LINK {
			let _ = tx_call_menu.send(UiCommand::CreateCallLink);
		} else if id == MENU_ID_JOIN_CALL_LINK {
			let _ = tx_call_menu.send(UiCommand::AskCallLink);
		}
	});

	let settings_dialog_shortcuts = parts.settings_dialog.dialog;
	let state_shortcuts = state.clone();
	let pending_shortcuts_edit = pending_shortcuts.clone();
	parts.settings_dialog.shortcuts_btn.on_click(move |_| {
		let current =
			pending_shortcuts_edit.borrow().clone().unwrap_or_else(|| (*state_shortcuts.borrow().keymap).clone());
		if let Some(updated) = wx_utils::shortcuts::prompt_for_shortcuts(&settings_dialog_shortcuts, &current) {
			*pending_shortcuts_edit.borrow_mut() = Some(updated);
		}
	});

	let tx_share_menu = ui_tx.clone();
	let share_menu_running = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
	parts.settings_dialog.share_menu_btn.on_click(move |_| {
		if share_menu_running.swap(true, std::sync::atomic::Ordering::SeqCst) {
			return;
		}
		let running = share_menu_running.clone();
		let tx = tx_share_menu.clone();
		std::thread::spawn(move || {
			let dir = std::env::current_exe().ok().and_then(|exe| exe.parent().map(|d| d.to_path_buf()));
			let result_file = dir.as_ref().map(|d| d.join("share").join("enable-result.txt"));
			if let Some(file) = &result_file {
				let _ = std::fs::remove_file(file);
			}
			let status = dir.and_then(|d| {
				std::process::Command::new(d.join("whatSnapShare.exe")).args(["--enable", "--quiet"]).status().ok()
			});
			let (ok, text) = match status {
				None => (
					false,
					"This copy of whatSnap doesn't include the Share menu helper. Reinstall whatSnap and try again."
						.to_string(),
				),
				Some(status) => (
					status.success(),
					result_file.and_then(|f| std::fs::read_to_string(f).ok()).unwrap_or_else(|| {
						"whatSnap couldn't be added to the Share menu. Try again, or reinstall whatSnap.".to_string()
					}),
				),
			};
			running.store(false, std::sync::atomic::Ordering::SeqCst);
			let _ = tx.send(UiCommand::ShareMenuResult { ok, text });
		});
	});

	let settings_dialog_cancel = parts.settings_dialog.dialog;
	parts.settings_dialog.cancel_btn.on_click(move |_| {
		settings_dialog_cancel.end_modal(wxdragon::id::ID_CANCEL);
	});

	let settings_dialog_backup = parts.settings_dialog.dialog;
	let settings_parts_backup = parts.settings_dialog;
	let state_backup = state.clone();
	let pending_shortcuts_backup = pending_shortcuts.clone();
	parts.settings_dialog.backup_btn.on_click(move |_| {
		let chooser = FileDialog::builder(&settings_dialog_backup)
			.with_message("Backup Settings")
			.with_default_file("whatsnap-settings.json")
			.with_wildcard("JSON files (*.json)|*.json|All files (*.*)|*.*")
			.with_style(FileDialogStyle::Save | FileDialogStyle::OverwritePrompt)
			.build();
		if chooser.show_modal() != wxdragon::id::ID_OK {
			return;
		}
		let Some(path) = chooser.get_path() else { return };

		let s = state_backup.borrow();
		let mut current = s.settings.clone();
		drop(s);

		settings_parts_backup.read_all_settings(&mut current);
		if let Some(keymap) = pending_shortcuts_backup.borrow().as_ref() {
			current.shortcuts = keymap.saved();
		}

		match current.export_json() {
			Ok(json_data) => {
				if let Err(e) = std::fs::write(&path, json_data) {
					MessageDialog::builder(
						&settings_dialog_backup,
						&format!("Failed to write backup file:\n{}", e),
						"Backup Failed",
					)
					.with_style(MessageDialogStyle::OK | MessageDialogStyle::IconError)
					.build()
					.show_modal();
				} else {
					MessageDialog::builder(
						&settings_dialog_backup,
						"Your whatSnap settings were successfully backed up.",
						"Backup Complete",
					)
					.with_style(MessageDialogStyle::OK | MessageDialogStyle::IconInformation)
					.build()
					.show_modal();
				}
			}
			Err(e) => {
				MessageDialog::builder(
					&settings_dialog_backup,
					&format!("Failed to format settings:\n{}", e),
					"Backup Failed",
				)
				.with_style(MessageDialogStyle::OK | MessageDialogStyle::IconError)
				.build()
				.show_modal();
			}
		}
	});

	let settings_dialog_restore = parts.settings_dialog.dialog;
	let settings_parts_restore = parts.settings_dialog;
	let state_restore = state.clone();
	let pending_shortcuts_restore = pending_shortcuts.clone();
	let frame_restore = parts.frame;
	parts.settings_dialog.restore_btn.on_click(move |_| {
		let chooser = FileDialog::builder(&settings_dialog_restore)
			.with_message("Restore Settings")
			.with_wildcard("JSON files (*.json)|*.json|All files (*.*)|*.*")
			.with_style(FileDialogStyle::Open | FileDialogStyle::FileMustExist)
			.build();
		if chooser.show_modal() != wxdragon::id::ID_OK {
			return;
		}
		let Some(path) = chooser.get_path() else { return };

		let raw = match std::fs::read_to_string(&path) {
			Ok(content) => content,
			Err(e) => {
				MessageDialog::builder(
					&settings_dialog_restore,
					&format!("Failed to read backup file:\n{}", e),
					"Restore Failed",
				)
				.with_style(MessageDialogStyle::OK | MessageDialogStyle::IconError)
				.build()
				.show_modal();
				return;
			}
		};

		let restored = match crate::settings::Settings::import_json(&raw) {
			Ok(settings) => settings,
			Err(err) => {
				MessageDialog::builder(
					&settings_dialog_restore,
					&format!("The selected file could not be restored:\n{}", err),
					"Invalid Settings File",
				)
				.with_style(MessageDialogStyle::OK | MessageDialogStyle::IconError)
				.build()
				.show_modal();
				return;
			}
		};

		let confirm = MessageDialog::builder(
			&settings_dialog_restore,
			"Restoring settings will overwrite your current settings. Do you want to continue?",
			"Restore Settings",
		)
		.with_style(MessageDialogStyle::YesNo | MessageDialogStyle::IconQuestion)
		.build();
		if confirm.show_modal() != wxdragon::id::ID_YES {
			return;
		}

		restored.save();

		let mut s = state_restore.borrow_mut();
		s.settings = restored.clone();
		settings_parts_restore.populate_from_settings(&s.settings);

		let keymap = crate::keymap::Keymap::from_saved(&s.settings.shortcuts);
		keymap.apply(&frame_restore);
		s.keymap = std::rc::Rc::new(keymap);
		pending_shortcuts_restore.borrow_mut().take();

		let theme = s.settings.theme;
		wxdragon::call_after(Box::new(move || crate::ui::theme::change_theme(theme)));

		crate::ui::spell_check::configure(s.settings.spell_check, &s.settings.spell_language);

		s.audio_out.stream = None;
		s.call_audio_out.stream = None;
		crate::ui_commands::calls::send_call_options(&s);

		let synced = s.settings.synced();
		let mut map = serde_json::Map::new();
		for (k, v) in synced {
			map.insert(k.to_string(), v);
		}
		if !map.is_empty() {
			let _ = s.ws_tx.send(serde_json::json!({"action": "sync_settings", "data": map}).to_string());
		}
		drop(s);

		MessageDialog::builder(
			&settings_dialog_restore,
			"Your whatSnap settings were successfully restored.",
			"Restore Complete",
		)
		.with_style(MessageDialogStyle::OK | MessageDialogStyle::IconInformation)
		.build()
		.show_modal();
	});

	for control in parts.settings_dialog.privacy_controls() {
		let Some(except_btn) = control.except_btn else { continue };
		let settings_parts = parts.settings_dialog;
		let name = control.name;
		let find = move || settings_parts.privacy_controls().into_iter().find(|c| c.name == name);
		control.combo.on_selection_changed(move |_| {
			if let Some(control) = find() {
				except_btn.enable(control.is_except());
			}
		});
		let state_except = state.clone();
		except_btn.on_click(move |_| {
			if let Some(control) = find() {
				choose_excluded_contacts(&state_except, settings_parts.dialog, &control);
			}
		});
	}

	// Log Out and Accounts leave Settings: what they lead to (the login
	// window, another account's chats) doesn't belong under it.
	let settings_dialog_logout = parts.settings_dialog.dialog;
	let state_logout_btn = state.clone();
	parts.settings_dialog.logout_btn.on_click(move |_| {
		if confirm_logout(&settings_dialog_logout, &state_logout_btn) {
			settings_dialog_logout.end_modal(wxdragon::id::ID_CANCEL);
		}
	});

	let settings_dialog_accounts = parts.settings_dialog.dialog;
	let tx_accounts_btn = ui_tx.clone();
	parts.settings_dialog.accounts_btn.on_click(move |_| {
		settings_dialog_accounts.end_modal(wxdragon::id::ID_CANCEL);
		let _ = tx_accounts_btn.send(UiCommand::OpenAccounts(None));
	});

	// Blocked Contacts opens over Settings (see blocking::show_blocked_contacts).
	let tx_blocked = ui_tx.clone();
	parts.settings_dialog.blocked_contacts_btn.on_click(move |_| {
		let _ = tx_blocked.send(UiCommand::ShowBlockedContacts);
	});

	let tx_status_privacy_settings = ui_tx.clone();
	parts.settings_dialog.status_privacy_btn.on_click(move |_| {
		let _ = tx_status_privacy_settings.send(UiCommand::OpenStatusPrivacy);
	});

	let settings_dialog_ok = parts.settings_dialog.dialog;
	let sounds_check_ok = parts.settings_dialog.sounds_check;
	let notifications_check_ok = parts.settings_dialog.notifications_check;
	let follow_dnd_check_ok = parts.settings_dialog.follow_dnd_check;
	let announce_typing_check_ok = parts.settings_dialog.announce_typing_check;
	let announce_recording_check_ok = parts.settings_dialog.announce_recording_check;
	let tray_icon_check_ok = parts.settings_dialog.tray_icon_check;
	let autostart_check_ok = parts.settings_dialog.autostart_check;
	let start_maximized_check_ok = parts.settings_dialog.start_maximized_check;
	let check_updates_check_ok = parts.settings_dialog.check_updates_check;
	let update_channel_combo_ok = parts.settings_dialog.update_channel_combo;
	let meta_ai_enabled_check_ok = parts.settings_dialog.meta_ai_enabled_check;
	let verbose_logging_check_ok = parts.settings_dialog.verbose_backend_logging_check;
	let name_field_ok = parts.settings_dialog.name_field;
	let settings_parts_ok = parts.settings_dialog;
	let input_device_combo_ok = parts.settings_dialog.input_device_combo;
	let output_device_combo_ok = parts.settings_dialog.output_device_combo;
	let call_output_device_combo_ok = parts.settings_dialog.call_output_device_combo;
	let noise_suppression_check_ok = parts.settings_dialog.voice_message_noise_suppression_check;
	let dual_stream_check_ok = parts.settings_dialog.dual_stream_screen_share_check;
	let state_settings_save = state.clone();
	let frame_shortcuts_save = parts.frame;
	parts.settings_dialog.ok_btn.on_click(move |_| {
		use crate::ui::dialogs::settings_dialog::*;
		let mut s = state_settings_save.borrow_mut();
		if s.privacy_settings_loaded {
			for control in settings_parts_ok.privacy_controls() {
				let already_except = s.privacy_settings.get(control.name).map(String::as_str) == Some(EXCEPT_VALUE);
				if control.is_except() && !already_except && !s.privacy_list_edits.contains_key(control.name) {
					drop(s);
					MessageDialog::builder(
						&settings_dialog_ok,
						&format!(
							"Choose the contacts excluded from {} first, with the button under it.",
							control.label
						),
						"Settings",
					)
					.with_style(MessageDialogStyle::OK | MessageDialogStyle::IconInformation)
					.build()
					.show_modal();
					if let Some(btn) = control.except_btn {
						btn.set_focus();
					}
					return;
				}
			}
		}
		if let Some(keymap) = pending_shortcuts.borrow_mut().take() {
			keymap.apply(&frame_shortcuts_save);
			s.settings.shortcuts = keymap.saved();
			s.keymap = Rc::new(keymap);
		}
		let synced_before = s.settings.synced();
		s.settings.sounds_enabled = sounds_check_ok.is_checked();
		s.settings.notifications_enabled = notifications_check_ok.is_checked();
		s.settings.follow_windows_do_not_disturb = follow_dnd_check_ok.is_checked();
		s.settings.announce_typing = announce_typing_check_ok.is_checked();
		s.settings.announce_recording = announce_recording_check_ok.is_checked();
		s.settings.show_tray_icon = tray_icon_check_ok.is_checked();
		s.settings.meta_ai_enabled = meta_ai_enabled_check_ok.is_checked();
		s.settings.verbose_backend_logging = verbose_logging_check_ok.is_checked();
		s.settings.start_maximized = start_maximized_check_ok.is_checked();
		s.settings.check_for_updates_on_startup = check_updates_check_ok.is_checked();
		s.settings.update_channel = crate::ui::dialogs::settings_dialog::combo_value(
			&update_channel_combo_ok,
			crate::ui::dialogs::settings_dialog::UPDATE_CHANNEL_CHOICES,
		);
		s.settings.voice_message_noise_suppression = noise_suppression_check_ok.is_checked();
		s.settings.dual_stream_screen_share = dual_stream_check_ok.is_checked();
		let old_theme = s.settings.theme;
		settings_parts_ok.read_chat_settings(&mut s.settings);
		crate::ui::spell_check::configure(s.settings.spell_check, &s.settings.spell_language);
		if s.settings.theme != old_theme {
			let theme = s.settings.theme;
			wxdragon::call_after(Box::new(move || theme::change_theme(theme)));
		}
		let changes = s.app_prefs.as_ref().map(|loaded| settings_parts_ok.app_prefs_changes(loaded));
		if let Some(changes) = changes.filter(|c| !c.is_empty()) {
			if let Some(prefs) = s.app_prefs.as_mut().and_then(|p| p.as_object_mut()) {
				prefs.extend(changes.clone());
			}
			let payload = serde_json::json!({"action": "set_app_prefs", "data": changes});
			let _ = s.ws_tx.send(payload.to_string());
		}
		crate::ui_commands::calls::send_call_options(&s);
		s.settings.input_device_name = device_combo_value(&input_device_combo_ok);
		let new_output_device = device_combo_value(&output_device_combo_ok);
		if new_output_device != s.settings.output_device_name {
			s.settings.output_device_name = new_output_device;
			s.audio_out.stream = None;
		}
		let new_call_output_device = device_combo_value(&call_output_device_combo_ok);
		if new_call_output_device != s.settings.call_output_device_name {
			s.settings.call_output_device_name = new_call_output_device;
			s.call_audio_out.stream = None;
		}
		s.settings.save();
		let mut synced = s.settings.synced_changes(&synced_before);
		if crate::platform::autostart::is_enabled() != autostart_check_ok.is_checked() {
			synced.insert("startAtLogin".into(), autostart_check_ok.is_checked().into());
		}
		if !synced.is_empty() {
			let _ = s.ws_tx.send(serde_json::json!({"action": "sync_settings", "data": synced}).to_string());
		}

		let new_name = name_field_ok.get_value();
		if new_name != s.own_push_name {
			s.own_push_name = new_name.clone();
			let _ =
				s.ws_tx.send(serde_json::json!({"action": "set_push_name", "data": {"name": new_name}}).to_string());
		}
		if s.own_about_loaded {
			let new_about = settings_parts_ok.about_field.get_value();
			let picked = combo_value(&settings_parts_ok.about_duration_combo, ABOUT_DURATION_CHOICES);
			let duration = if picked == shown_value(ABOUT_DURATION_CHOICES, &s.own_about_duration.to_string()) {
				s.own_about_duration
			} else {
				picked.parse().unwrap_or(-1)
			};
			if new_about != s.own_about || duration != s.own_about_duration {
				s.own_about = new_about.clone();
				s.own_about_duration = duration;
				let payload =
					serde_json::json!({"action": "set_about", "data": {"text": new_about, "duration": duration}});
				let _ = s.ws_tx.send(payload.to_string());
			}
		}
		if let Some(current) = s.default_timer {
			let picked = combo_value(&settings_parts_ok.default_timer_combo, DEFAULT_TIMER_CHOICES);
			if picked != shown_value(DEFAULT_TIMER_CHOICES, &current.to_string()) {
				let seconds: i64 = picked.parse().unwrap_or(0);
				s.default_timer = Some(seconds);
				let payload = serde_json::json!({"action": "set_default_timer", "data": {"seconds": seconds}});
				let _ = s.ws_tx.send(payload.to_string());
			}
		}
		if s.privacy_settings_loaded
			&& let Some(value) = settings_parts_ok.read_receipts_change(&s.privacy_settings)
		{
			s.privacy_settings.insert("readreceipts".to_string(), value.to_string());
			let payload =
				serde_json::json!({"action": "set_privacy_setting", "data": {"name": "readreceipts", "value": value}});
			let _ = s.ws_tx.send(payload.to_string());
		}
		if s.privacy_settings_loaded {
			for control in settings_parts_ok.privacy_controls() {
				let server = s.privacy_settings.get(control.name).cloned().unwrap_or_default();
				if control.is_except() {
					if let Some(jids) = s.privacy_list_edits.get(control.name) {
						let payload = serde_json::json!({"action": "set_privacy_list", "data": {"name": control.name, "jids": jids}});
						let _ = s.ws_tx.send(payload.to_string());
					}
					continue;
				}
				let value = combo_value(&control.combo, control.choices);
				if value != shown_value(control.choices, &server) {
					s.privacy_settings.insert(control.name.to_string(), value.clone());
					let payload = serde_json::json!({"action": "set_privacy_setting", "data": {"name": control.name, "value": value}});
					let _ = s.ws_tx.send(payload.to_string());
				}
			}
		}
		let logged_in = s.main_ui_shown;
		drop(s);
		// Respect the setting, but never surface the icon while logged out.
		apply_tray_setting(tray_icon_check_ok.is_checked() && logged_in);
		if crate::platform::autostart::is_enabled() != autostart_check_ok.is_checked() {
			crate::platform::autostart::set_enabled(autostart_check_ok.is_checked());
		}
		settings_dialog_ok.end_modal(wxdragon::id::ID_OK);
	});

	let state_photo = state.clone();
	let settings_dialog_photo = parts.settings_dialog.dialog;
	parts.settings_dialog.change_photo_btn.on_click(move |_| {
		let file_dialog = FileDialog::builder(&settings_dialog_photo)
			.with_message("Choose a profile photo")
			.with_wildcard("Pictures (*.jpg;*.jpeg;*.png;*.webp;*.bmp;*.gif)|*.jpg;*.jpeg;*.png;*.webp;*.bmp;*.gif")
			.with_style(FileDialogStyle::Open | FileDialogStyle::FileMustExist)
			.build();
		if file_dialog.show_modal() != wxdragon::id::ID_OK {
			return;
		}
		let Some(path) = file_dialog.get_path() else { return };
		let payload = serde_json::json!({"action": "set_own_photo", "data": {"path": path}});
		let _ = state_photo.borrow().ws_tx.send(payload.to_string());
	});
	let state_photo_remove = state.clone();
	parts.settings_dialog.remove_photo_btn.on_click(move |_| {
		let dialog = MessageDialog::builder(&settings_dialog_photo, "Remove your profile photo?", "Profile Photo")
			.with_style(MessageDialogStyle::YesNo | MessageDialogStyle::IconQuestion)
			.build();
		dialog.set_yes_no_labels("Remove", "Cancel");
		if dialog.show_modal() == ID_YES {
			let payload = serde_json::json!({"action": "set_own_photo", "data": {"remove": true}});
			let _ = state_photo_remove.borrow().ws_tx.send(payload.to_string());
		}
	});

	let tx_group_settings = ui_tx.clone();
	let group_name_field = parts.group_data_dialog.group_name_field;
	let group_desc_field = parts.group_data_dialog.group_desc_field;
	let announce_chk = parts.group_data_dialog.announce_chk;
	let locked_chk = parts.group_data_dialog.locked_chk;
	let forwarded_chk = parts.group_data_dialog.forwarded_chk;
	let admin_reports_chk = parts.group_data_dialog.admin_reports_chk;
	let history_chk = parts.group_data_dialog.history_chk;
	let admins_add_chk = parts.group_data_dialog.admins_add_chk;
	let approve_members_chk = parts.group_data_dialog.approve_members_chk;
	parts.group_data_dialog.save_settings_btn.on_click(move |_| {
		let name = group_name_field.get_value();
		let topic = group_desc_field.get_value();
		let announce = announce_chk.is_checked();
		let locked = locked_chk.is_checked();
		let _ = tx_group_settings.send(UiCommand::UpdateGroupSettings { name, topic, announce, locked });
		let _ = tx_group_settings.send(UiCommand::UpdateGroupModeration {
			no_frequently_forwarded: forwarded_chk.is_checked(),
			allow_admin_reports: admin_reports_chk.is_checked(),
			group_history: history_chk.is_checked(),
			admins_add_only: admins_add_chk.is_checked(),
			approve_members: approve_members_chk.is_checked(),
		});
	});

	let gd = &parts.group_data_dialog;
	for (button, action) in [
		(gd.join_requests_btn, "join_requests"),
		(gd.link_group_btn, "link_group"),
		(gd.unlink_group_btn, "unlink_group"),
		(gd.deactivate_btn, "deactivate_community"),
	] {
		let tx = ui_tx.clone();
		button.on_click(move |_| {
			let _ = tx.send(UiCommand::GroupAction(action));
		});
	}

	let tx_set_tag = ui_tx.clone();
	let tag_field = parts.group_data_dialog.my_tag_field;
	let send_tag = move || {
		let _ = tx_set_tag.send(UiCommand::SetMemberLabel(tag_field.get_value().trim().to_string()));
	};
	let send_tag_enter = send_tag.clone();
	parts.group_data_dialog.set_tag_btn.on_click(move |_| send_tag());
	parts.group_data_dialog.my_tag_field.on_text_enter(move |_| send_tag_enter());

	let tx_reported = ui_tx.clone();
	parts.group_data_dialog.reported_btn.on_click(move |_| {
		let _ = tx_reported.send(UiCommand::ShowReportedMessages);
	});

	let tx_group_members_search = ui_tx.clone();
	let members_search = parts.group_data_dialog.members_search;
	members_search.on_text_changed(move |_| {
		let query = members_search.get_value();
		let _ = tx_group_members_search.send(UiCommand::FilterGroupMembers(query));
	});

	let tx_add_member = ui_tx.clone();
	parts.group_data_dialog.add_member_btn.on_click(move |_| {
		let _ = tx_add_member.send(UiCommand::PrepareAddMembers);
	});

	wire_group_member_menu(parts, &ui_tx, state);

	let tx_member_changes = ui_tx.clone();
	parts.group_data_dialog.member_changes_btn.on_click(move |_| {
		let _ = tx_member_changes.send(UiCommand::ShowMemberChanges);
	});

	let tx_invite_link = ui_tx.clone();
	parts.group_data_dialog.invite_link_btn.on_click(move |_| {
		let _ = tx_invite_link.send(UiCommand::ShowGroupInviteLink);
	});

	let tx_join_group = ui_tx.clone();
	let groups_list_join = parts.group_data_dialog.groups_list;
	parts.group_data_dialog.join_group_btn.on_click(move |_| {
		if let Some(idx) = groups_list_join.get_selection() {
			let _ = tx_join_group.send(UiCommand::JoinCommunitySubgroup(idx));
		}
	});

	let tx_common_click = ui_tx.clone();
	parts.conversation_data_dialog.common_groups_list.on_item_double_clicked(move |event| {
		if let Some(idx) = event.get_selection() {
			let _ = tx_common_click.send(UiCommand::OpenCommonGroup(idx as usize));
		}
	});
	let tx_common_enter = ui_tx.clone();
	let common_list_enter = parts.conversation_data_dialog.common_groups_list;
	keys::on_enter(&parts.conversation_data_dialog.common_groups_list, Rc::default(), move || {
		if let Some(idx) = common_list_enter.get_selection() {
			let _ = tx_common_enter.send(UiCommand::OpenCommonGroup(idx as usize));
		}
	});
	let tx_chat_filter = ui_tx.clone();
	parts.conversations_parts.chat_filter_combo.on_selection_changed(move |event| {
		if let Some(idx) = event.get_selection() {
			let _ = tx_chat_filter.send(UiCommand::SetChatFilter(idx as usize));
		}
	});

	let tx_group_selection = ui_tx.clone();
	parts.group_data_dialog.groups_list.on_selection_changed(move |_| {
		let _ = tx_group_selection.send(UiCommand::CommunitySubgroupSelectionChanged);
	});
	let tx_group_double_click = ui_tx.clone();
	parts.group_data_dialog.groups_list.on_item_double_clicked(move |event| {
		if let Some(idx) = event.get_selection() {
			let _ = tx_group_double_click.send(UiCommand::JoinCommunitySubgroup(idx as u32));
		}
	});
	let tx_group_enter = ui_tx.clone();
	let groups_list_enter = parts.group_data_dialog.groups_list;
	keys::on_enter(&parts.group_data_dialog.groups_list, Rc::default(), move || {
		if let Some(idx) = groups_list_enter.get_selection() {
			let _ = tx_group_enter.send(UiCommand::JoinCommunitySubgroup(idx));
		}
	});

	wire_media_list(
		&parts.group_data_dialog.media_list,
		&parts.group_data_dialog.dialog,
		&ui_tx,
		state,
		|s, i| s.group_data_media.get(i).cloned(),
		true,
	);
	wire_media_list(
		&parts.group_data_dialog.documents_list,
		&parts.group_data_dialog.dialog,
		&ui_tx,
		state,
		|s, i| s.group_data_documents.get(i).cloned(),
		true,
	);
	wire_media_list(
		&parts.group_data_dialog.links_list,
		&parts.group_data_dialog.dialog,
		&ui_tx,
		state,
		|s, i| s.group_data_links.get(i).cloned(),
		false,
	);

	wire_media_list(
		&parts.conversation_data_dialog.media_list,
		&parts.conversation_data_dialog.dialog,
		&ui_tx,
		state,
		|s, i| s.conversation_data_media.get(i).cloned(),
		true,
	);
	wire_media_list(
		&parts.conversation_data_dialog.documents_list,
		&parts.conversation_data_dialog.dialog,
		&ui_tx,
		state,
		|s, i| s.conversation_data_documents.get(i).cloned(),
		true,
	);
	wire_media_list(
		&parts.conversation_data_dialog.links_list,
		&parts.conversation_data_dialog.dialog,
		&ui_tx,
		state,
		|s, i| s.conversation_data_links.get(i).cloned(),
		false,
	);

	let poll_dialog_cancel = parts.poll_dialog.dialog;
	parts.poll_dialog.cancel_btn.on_click(move |_| {
		poll_dialog_cancel.end_modal(ID_CANCEL);
	});
	let tx_poll = ui_tx.clone();
	let poll_dlg = parts.poll_dialog.dialog;
	let poll_question = parts.poll_dialog.question_field;
	let poll_options = parts.poll_dialog.options_field;
	let poll_multiple = parts.poll_dialog.multiple_check;
	let poll_quiz = parts.poll_dialog.quiz_check;
	parts.poll_dialog.send_btn.on_click(move |_| {
		let question = poll_question.get_value();
		match whatsnap::validate_poll(&question, &poll_options.get_value()) {
			Ok(options) => {
				let quiz = poll_quiz.get_value();
				let correct_answer = if quiz {
					let labels: Vec<&str> = options.iter().map(String::as_str).collect();
					let choose =
						SingleChoiceDialog::builder(&poll_dlg, "Which answer is correct?", "Quiz", &labels).build();
					if choose.show_modal() != ID_OK {
						return;
					}
					let Some(answer) = choose.get_string_selection() else { return };
					Some(answer)
				} else {
					None
				};
				poll_dlg.end_modal(ID_OK);
				let _ = tx_poll.send(UiCommand::SendPoll {
					question: question.trim().to_string(),
					options,
					multiple_answers: !quiz && poll_multiple.get_value(),
					correct_answer,
				});
			}
			Err(problem) => {
				unsafe {
					let _ = windows::Win32::System::Diagnostics::Debug::MessageBeep(
						windows::Win32::UI::WindowsAndMessaging::MB_OK,
					);
				}
				MessageDialog::builder(&poll_dlg, problem, "Poll")
					.with_style(MessageDialogStyle::OK | MessageDialogStyle::IconError)
					.build()
					.show_modal();
				if question.trim().is_empty() || question.trim().chars().count() > whatsnap::POLL_MAX_QUESTION_CHARS {
					poll_question.set_focus();
				} else {
					poll_options.set_focus();
				}
			}
		}
	});

	let new_contact_dialog_cancel = parts.new_contact_dialog.dialog;
	parts.new_contact_dialog.cancel_btn.on_click(move |_| {
		new_contact_dialog_cancel.end_modal(0);
	});

	let tx_new_contact = ui_tx.clone();
	let dlg = parts.new_contact_dialog.dialog;
	let name_field = parts.new_contact_dialog.name_field;
	let surname_field = parts.new_contact_dialog.surname_field;
	let phone_field = parts.new_contact_dialog.phone_field;
	let sync_check = parts.new_contact_dialog.sync_check;
	parts.new_contact_dialog.ok_btn.on_click(move |_| {
		let name = name_field.get_value().trim().to_string();
		let surname = surname_field.get_value().trim().to_string();
		let phone = phone_field.get_value();
		if name.is_empty() {
			unsafe {
				let _ = windows::Win32::System::Diagnostics::Debug::MessageBeep(
					windows::Win32::UI::WindowsAndMessaging::MB_OK,
				);
			}
			let owner = windows::Win32::Foundation::HWND(dlg.get_handle());
			crate::platform::focus::show_modal_and_refocus(owner, false, || {
				MessageDialog::builder(&dlg, "Name is required.", "Error")
					.with_style(MessageDialogStyle::OK | MessageDialogStyle::IconError)
					.build()
					.show_modal()
			});
			name_field.set_focus();
			return;
		}
		let clean_phone: String = phone.chars().filter(|c| c.is_ascii_digit()).collect();
		if clean_phone.len() < 7 {
			unsafe {
				let _ = windows::Win32::System::Diagnostics::Debug::MessageBeep(
					windows::Win32::UI::WindowsAndMessaging::MB_OK,
				);
			}
			let owner = windows::Win32::Foundation::HWND(dlg.get_handle());
			crate::platform::focus::show_modal_and_refocus(owner, false, || {
				MessageDialog::builder(&dlg, "Phone number must have at least 7 digits.", "Error")
					.with_style(MessageDialogStyle::OK | MessageDialogStyle::IconError)
					.build()
					.show_modal()
			});
			phone_field.set_focus();
			return;
		}
		let full_name = format!("{} {}", name, surname).trim().to_string();
		let sync = sync_check.get_value();
		dlg.end_modal(wxdragon::id::ID_OK);
		let _ = tx_new_contact.send(UiCommand::NewContact {
			name: full_name,
			first_name: name,
			phone: phone.trim().to_string(),
			sync,
		});
	});

	let tx_prepare = ui_tx.clone();
	let new_conv_dlg_for_group = parts.new_conv_dialog.dialog;
	parts.new_conv_dialog.new_group_btn.on_click(move |_| {
		new_conv_dlg_for_group.end_modal(0);
		let _ = tx_prepare.send(UiCommand::PrepareNewGroup);
	});

	let tx_create = ui_tx.clone();
	let group_dialog = parts.new_group_dialog.dialog;
	let name_ctrl = parts.new_group_dialog.name_field;
	let extra_ctrl = parts.new_group_dialog.extra_number_field;
	let contacts_listbox_ok = parts.new_group_dialog.contacts_listbox;
	let state_ok = state.clone();
	let ok_btn = parts.new_group_dialog.ok_btn;

	parts.new_group_dialog.ok_btn.on_click(move |_| {
		let name = name_ctrl.get_value().trim().to_string();
		if name.is_empty() {
			unsafe {
				let _ = windows::Win32::System::Diagnostics::Debug::MessageBeep(
					windows::Win32::UI::WindowsAndMessaging::MESSAGEBOX_STYLE(0x00000030),
				);
			}
			let owner = windows::Win32::Foundation::HWND(group_dialog.get_handle());
			crate::platform::focus::show_modal_and_refocus(owner, false, || {
				MessageDialog::builder(&group_dialog, "Group name is required.", "Error")
					.with_style(MessageDialogStyle::OK | MessageDialogStyle::IconError)
					.build()
					.show_modal()
			});
			name_ctrl.select_all();
			name_ctrl.set_focus();
			return;
		}
		let mut s = state_ok.borrow_mut();
		let gs = &mut s.group_state;
		let count = contacts_listbox_ok.get_count();
		for idx in 0..count {
			if idx < gs.current_jids.len() as u32 {
				let jid = gs.current_jids[idx as usize].clone();
				if contacts_listbox_ok.is_checked(idx) {
					gs.checked_jids.insert(jid);
				} else {
					gs.checked_jids.remove(&jid);
				}
			}
		}
		let mut participants = Vec::new();
		for jid in &gs.checked_jids {
			participants.push(jid.clone());
		}
		drop(s);
		let extra_raw = extra_ctrl.get_value();
		for part in extra_raw.split([',', '\n']) {
			let digits: String = part.chars().filter(|c| c.is_ascii_digit()).collect();
			if digits.len() >= 7 {
				participants.push(digits);
			}
		}
		let mut seen = std::collections::HashSet::new();
		let mut unique_participants = Vec::new();
		for p in participants {
			if seen.insert(p.clone()) {
				unique_participants.push(p);
			}
		}
		if unique_participants.is_empty() {
			unsafe {
				let _ = windows::Win32::System::Diagnostics::Debug::MessageBeep(
					windows::Win32::UI::WindowsAndMessaging::MESSAGEBOX_STYLE(0x00000030),
				);
			}
			let owner = windows::Win32::Foundation::HWND(group_dialog.get_handle());
			crate::platform::focus::show_modal_and_refocus(owner, false, || {
				MessageDialog::builder(&group_dialog, "At least one participant must be selected.", "Error")
					.with_style(MessageDialogStyle::OK | MessageDialogStyle::IconError)
					.build()
					.show_modal()
			});
			contacts_listbox_ok.set_focus();
			return;
		}
		ok_btn.enable(false);
		group_dialog.end_modal(wxdragon::id::ID_OK);
		let _ = tx_create.send(UiCommand::CreateGroup { name, participants: unique_participants });
	});

	let new_group_cancel = parts.new_group_dialog.dialog;
	parts.new_group_dialog.cancel_btn.on_click(move |_| {
		new_group_cancel.end_modal(0);
	});

	let search_field = parts.new_group_dialog.search_field;
	let contacts_listbox = parts.new_group_dialog.contacts_listbox;
	let selection_announcer_search = parts.new_group_dialog.selection_announcer;
	let state_search = state.clone();
	// wxCheckListBox::Check() fires an OBJECT_STATECHANGE WinEvent per item
	// (checklst.cpp), which NVDA processes asynchronously. Rebuilding the
	// whole list (clear() + append() + re-Check()) on every single keystroke
	// queued events referencing items that a following keystroke's rebuild
	// had already replaced by the time NVDA got to them, landing NVDA on an
	// index past the new, shorter list and tripping wx's "bad
	// wxCheckListBox index" assert in IsChecked(). Debouncing so the list
	// only rebuilds once typing pauses keeps rebuilds far enough apart for
	// NVDA to finish querying the previous one first. This timer is shared
	// (via `parts.new_group_contacts_timer`) with a fresh `contacts` response
	// landing while the dialog is open, for the same reason - see the
	// "contacts" case in main.rs's websocket message handler.
	let group_search_timer = parts.new_group_contacts_timer.clone();
	let timer_for_tick = group_search_timer.clone();
	timer_for_tick.on_tick(move |_| {
		let query = search_field.get_value().to_lowercase().trim().to_string();
		let mut s = state_search.borrow_mut();
		let gs = &mut s.group_state;
		let count = contacts_listbox.get_count();
		for idx in 0..count {
			if idx < gs.current_jids.len() as u32 {
				let jid = gs.current_jids[idx as usize].clone();
				if contacts_listbox.is_checked(idx) {
					gs.checked_jids.insert(jid);
				} else {
					gs.checked_jids.remove(&jid);
				}
			}
		}
		let mut filtered_labels = Vec::new();
		let mut filtered_jids = Vec::new();
		for (lbl, jid) in gs.all_contact_labels.iter().zip(gs.all_contact_jids.iter()) {
			if query.is_empty() || lbl.to_lowercase().contains(&query) {
				filtered_labels.push(lbl.clone());
				filtered_jids.push(jid.clone());
			}
		}
		contacts_listbox.clear();
		for lbl in &filtered_labels {
			contacts_listbox.append(lbl);
		}
		gs.current_jids = filtered_jids;
		for (idx, jid) in gs.current_jids.iter().enumerate() {
			if gs.checked_jids.contains(jid) {
				contacts_listbox.check(idx as u32, true);
			}
		}
		// wxListBox::SetSelection asserts/crashes if given index 0 on an
		// empty list (only wxNOT_FOUND, which wxdragon's u32 API can't
		// express, is valid there) - so there's nothing to reset when the
		// filter has no matches, only when it has at least one.
		if !gs.current_jids.is_empty() {
			contacts_listbox.set_selection(0, true);
		}
		if !gs.checked_jids.is_empty() {
			let count = gs.checked_jids.len();
			crate::platform::announce::announce(
				selection_announcer_search,
				&format!(
					"{} shown. {} contact{} selected.",
					gs.current_jids.len(),
					count,
					if count == 1 { "" } else { "s" }
				),
			);
		}
	});
	search_field.on_text_changed(move |_| {
		group_search_timer.stop();
		group_search_timer.start(250, true);
	});

	let contacts_listbox_toggle = parts.new_group_dialog.contacts_listbox;
	let selection_announcer_toggle = parts.new_group_dialog.selection_announcer;
	let state_toggle = state.clone();
	contacts_listbox_toggle.on_toggled(move |event| {
		if let Some(idx) = event.get_selection() {
			let mut s = state_toggle.borrow_mut();
			let gs = &mut s.group_state;
			if idx < gs.current_jids.len() as u32 {
				let jid = gs.current_jids[idx as usize].clone();
				let checked = contacts_listbox_toggle.is_checked(idx);
				if checked {
					gs.checked_jids.insert(jid);
				} else {
					gs.checked_jids.remove(&jid);
				}
				let label = contacts_listbox_toggle.get_string(idx as usize).unwrap_or_default();
				let state_str = if checked { "selected" } else { "not selected" };
				let count = gs.checked_jids.len();
				crate::platform::announce::announce(
					selection_announcer_toggle,
					&format!(
						"{}: {}. {} contact{} selected.",
						label,
						state_str,
						count,
						if count == 1 { "" } else { "s" }
					),
				);
			}
		}
	});

	// Add Members dialog - mirrors the New Group dialog's checkbox-list
	// wiring above (including the debounced search, shared with a fresh
	// `contacts` response, and the crash fixes that motivated it), but
	// confirms with an explicit Add button instead of creating anything.
	let add_members_search_field = parts.add_members_dialog.search_field;
	let add_members_listbox = parts.add_members_dialog.contacts_listbox;
	let add_members_selection_announcer = parts.add_members_dialog.selection_announcer;
	let state_add_members_search = state.clone();
	let add_members_search_timer = parts.add_members_contacts_timer.clone();
	let add_members_timer_tick = add_members_search_timer.clone();
	add_members_timer_tick.on_tick(move |_| {
		let query = add_members_search_field.get_value().to_lowercase().trim().to_string();
		let mut s = state_add_members_search.borrow_mut();
		let ams = &mut s.add_members_state;
		let count = add_members_listbox.get_count();
		for idx in 0..count {
			if idx < ams.current_jids.len() as u32 {
				let jid = ams.current_jids[idx as usize].clone();
				if add_members_listbox.is_checked(idx) {
					ams.checked_jids.insert(jid);
				} else {
					ams.checked_jids.remove(&jid);
				}
			}
		}
		let mut filtered_labels = Vec::new();
		let mut filtered_jids = Vec::new();
		for (lbl, jid) in ams.all_contact_labels.iter().zip(ams.all_contact_jids.iter()) {
			if query.is_empty() || lbl.to_lowercase().contains(&query) {
				filtered_labels.push(lbl.clone());
				filtered_jids.push(jid.clone());
			}
		}
		add_members_listbox.clear();
		for lbl in &filtered_labels {
			add_members_listbox.append(lbl);
		}
		ams.current_jids = filtered_jids;
		for (idx, jid) in ams.current_jids.iter().enumerate() {
			if ams.checked_jids.contains(jid) {
				add_members_listbox.check(idx as u32, true);
			}
		}
		if !ams.current_jids.is_empty() {
			add_members_listbox.set_selection(0, true);
		}
		if !ams.checked_jids.is_empty() {
			let count = ams.checked_jids.len();
			crate::platform::announce::announce(
				add_members_selection_announcer,
				&format!(
					"{} shown. {} contact{} selected.",
					ams.current_jids.len(),
					count,
					if count == 1 { "" } else { "s" }
				),
			);
		}
	});
	add_members_search_field.on_text_changed(move |_| {
		add_members_search_timer.stop();
		add_members_search_timer.start(250, true);
	});

	let add_members_listbox_toggle = parts.add_members_dialog.contacts_listbox;
	let add_members_selection_announcer_toggle = parts.add_members_dialog.selection_announcer;
	let state_add_members_toggle = state.clone();
	add_members_listbox_toggle.on_toggled(move |event| {
		if let Some(idx) = event.get_selection() {
			let mut s = state_add_members_toggle.borrow_mut();
			let ams = &mut s.add_members_state;
			if idx < ams.current_jids.len() as u32 {
				let jid = ams.current_jids[idx as usize].clone();
				let checked = add_members_listbox_toggle.is_checked(idx);
				if checked {
					ams.checked_jids.insert(jid);
				} else {
					ams.checked_jids.remove(&jid);
				}
				let label = add_members_listbox_toggle.get_string(idx as usize).unwrap_or_default();
				let state_str = if checked { "selected" } else { "not selected" };
				let count = ams.checked_jids.len();
				crate::platform::announce::announce(
					add_members_selection_announcer_toggle,
					&format!(
						"{}: {}. {} contact{} selected.",
						label,
						state_str,
						count,
						if count == 1 { "" } else { "s" }
					),
				);
			}
		}
	});

	let tx_add_members_confirm = ui_tx.clone();
	let add_members_listbox_ok = parts.add_members_dialog.contacts_listbox;
	let add_members_dialog_ok = parts.add_members_dialog.dialog;
	let state_add_members_ok = state.clone();
	parts.add_members_dialog.add_btn.on_click(move |_| {
		let mut s = state_add_members_ok.borrow_mut();
		let ams = &mut s.add_members_state;
		let count = add_members_listbox_ok.get_count();
		for idx in 0..count {
			if idx < ams.current_jids.len() as u32 {
				let jid = ams.current_jids[idx as usize].clone();
				if add_members_listbox_ok.is_checked(idx) {
					ams.checked_jids.insert(jid);
				} else {
					ams.checked_jids.remove(&jid);
				}
			}
		}
		let participants: Vec<String> = ams.checked_jids.iter().cloned().collect();
		drop(s);
		if participants.is_empty() {
			unsafe {
				let _ = windows::Win32::System::Diagnostics::Debug::MessageBeep(
					windows::Win32::UI::WindowsAndMessaging::MESSAGEBOX_STYLE(0x00000030),
				);
			}
			let owner = windows::Win32::Foundation::HWND(add_members_dialog_ok.get_handle());
			crate::platform::focus::show_modal_and_refocus(owner, false, || {
				MessageDialog::builder(&add_members_dialog_ok, "At least one contact must be selected.", "Error")
					.with_style(MessageDialogStyle::OK | MessageDialogStyle::IconError)
					.build()
					.show_modal()
			});
			add_members_listbox_ok.set_focus();
			return;
		}
		add_members_dialog_ok.end_modal(wxdragon::id::ID_OK);
		let _ = tx_add_members_confirm.send(UiCommand::AddGroupMembers(participants));
	});

	let add_members_cancel = parts.add_members_dialog.dialog;
	parts.add_members_dialog.cancel_btn.on_click(move |_| {
		add_members_cancel.end_modal(0);
	});

	let attach_contact_search_field = parts.attach_contact_dialog.search_field;
	let attach_contact_listbox = parts.attach_contact_dialog.contacts_listbox;
	let attach_contact_selection_announcer = parts.attach_contact_dialog.selection_announcer;
	let state_attach_contact_search = state.clone();
	let attach_contact_search_timer = parts.attach_contact_timer.clone();
	let attach_contact_timer_tick = attach_contact_search_timer.clone();
	attach_contact_timer_tick.on_tick(move |_| {
		let query = attach_contact_search_field.get_value().to_lowercase().trim().to_string();
		let mut s = state_attach_contact_search.borrow_mut();
		let acs = &mut s.attach_contact_state;
		let count = attach_contact_listbox.get_count();
		for idx in 0..count {
			if idx < acs.current_jids.len() as u32 {
				let jid = acs.current_jids[idx as usize].clone();
				if attach_contact_listbox.is_checked(idx) {
					acs.checked_jids.insert(jid);
				} else {
					acs.checked_jids.remove(&jid);
				}
			}
		}
		let mut filtered_labels = Vec::new();
		let mut filtered_jids = Vec::new();
		for (lbl, jid) in acs.all_contact_labels.iter().zip(acs.all_contact_jids.iter()) {
			if query.is_empty() || lbl.to_lowercase().contains(&query) {
				filtered_labels.push(lbl.clone());
				filtered_jids.push(jid.clone());
			}
		}
		attach_contact_listbox.clear();
		for lbl in &filtered_labels {
			attach_contact_listbox.append(lbl);
		}
		acs.current_jids = filtered_jids;
		for (idx, jid) in acs.current_jids.iter().enumerate() {
			if acs.checked_jids.contains(jid) {
				attach_contact_listbox.check(idx as u32, true);
			}
		}
		if !acs.current_jids.is_empty() {
			attach_contact_listbox.set_selection(0, true);
		}
		if !acs.checked_jids.is_empty() {
			let count = acs.checked_jids.len();
			crate::platform::announce::announce(
				attach_contact_selection_announcer,
				&format!(
					"{} shown. {} contact{} selected.",
					acs.current_jids.len(),
					count,
					if count == 1 { "" } else { "s" }
				),
			);
		}
	});
	attach_contact_search_field.on_text_changed(move |_| {
		attach_contact_search_timer.stop();
		attach_contact_search_timer.start(250, true);
	});

	let attach_contact_listbox_toggle = parts.attach_contact_dialog.contacts_listbox;
	let attach_contact_selection_announcer_toggle = parts.attach_contact_dialog.selection_announcer;
	let state_attach_contact_toggle = state.clone();
	attach_contact_listbox_toggle.on_toggled(move |event| {
		if let Some(idx) = event.get_selection() {
			let mut s = state_attach_contact_toggle.borrow_mut();
			let acs = &mut s.attach_contact_state;
			if idx < acs.current_jids.len() as u32 {
				let jid = acs.current_jids[idx as usize].clone();
				let checked = attach_contact_listbox_toggle.is_checked(idx);
				if checked {
					acs.checked_jids.insert(jid);
				} else {
					acs.checked_jids.remove(&jid);
				}
				let label = attach_contact_listbox_toggle.get_string(idx as usize).unwrap_or_default();
				let state_str = if checked { "selected" } else { "not selected" };
				let count = acs.checked_jids.len();
				crate::platform::announce::announce(
					attach_contact_selection_announcer_toggle,
					&format!(
						"{}: {}. {} contact{} selected.",
						label,
						state_str,
						count,
						if count == 1 { "" } else { "s" }
					),
				);
			}
		}
	});

	let tx_attach_contact_confirm = ui_tx.clone();
	let attach_contact_listbox_ok = parts.attach_contact_dialog.contacts_listbox;
	let attach_contact_dialog_ok = parts.attach_contact_dialog.dialog;
	let state_attach_contact_ok = state.clone();
	parts.attach_contact_dialog.send_btn.on_click(move |_| {
		let mut s = state_attach_contact_ok.borrow_mut();
		let acs = &mut s.attach_contact_state;
		let count = attach_contact_listbox_ok.get_count();
		for idx in 0..count {
			if idx < acs.current_jids.len() as u32 {
				let jid = acs.current_jids[idx as usize].clone();
				if attach_contact_listbox_ok.is_checked(idx) {
					acs.checked_jids.insert(jid);
				} else {
					acs.checked_jids.remove(&jid);
				}
			}
		}
		let contact_jids: Vec<String> = acs.checked_jids.iter().cloned().collect();
		drop(s);
		if contact_jids.is_empty() {
			unsafe {
				let _ = windows::Win32::System::Diagnostics::Debug::MessageBeep(
					windows::Win32::UI::WindowsAndMessaging::MESSAGEBOX_STYLE(0x00000030),
				);
			}
			let owner = windows::Win32::Foundation::HWND(attach_contact_dialog_ok.get_handle());
			crate::platform::focus::show_modal_and_refocus(owner, false, || {
				MessageDialog::builder(&attach_contact_dialog_ok, "At least one contact must be selected.", "Error")
					.with_style(MessageDialogStyle::OK | MessageDialogStyle::IconError)
					.build()
					.show_modal()
			});
			attach_contact_listbox_ok.set_focus();
			return;
		}
		attach_contact_dialog_ok.end_modal(wxdragon::id::ID_OK);
		let _ = tx_attach_contact_confirm.send(UiCommand::SendContacts(contact_jids));
	});

	let attach_contact_cancel = parts.attach_contact_dialog.dialog;
	parts.attach_contact_dialog.cancel_btn.on_click(move |_| {
		attach_contact_cancel.end_modal(0);
	});

	let tx_nav = ui_tx.clone();
	let state_nav = state.clone();
	parts.nav_list.on_selection_changed(move |event| {
		if let Some(idx) = event.get_selection() {
			let tab = state_nav.borrow().settings.tab_order().get(idx as usize).copied().unwrap_or(idx as usize);
			let _ = tx_nav.send(UiCommand::NavSelected(tab));
		}
	});

	let state_nav_move = state.clone();
	let nav_list_move = parts.nav_list;
	let frame_nav_move = parts.frame;
	let nav_move_announcer = parts.conversations_parts.messages_announcer;
	parts.nav_list.on_key_down(move |event| {
		let WindowEventData::Keyboard(ref key) = event else {
			event.skip(true);
			return;
		};
		let code = key.get_key_code();
		let delta: isize = if crate::ui::keys::is_left(code) {
			-1
		} else if crate::ui::keys::is_right(code) {
			1
		} else {
			event.skip(true);
			return;
		};
		if !(key.control_down() && key.shift_down()) || key.alt_down() {
			event.skip(true);
			return;
		}
		event.skip(false);
		let Some(pos) = nav_list_move.get_selection().map(|p| p as usize) else { return };
		let mut order = state_nav_move.borrow().settings.tab_order();
		let Some(new_pos) = crate::settings::move_tab(&mut order, pos, delta) else {
			crate::platform::announce::announce(
				nav_move_announcer,
				if delta < 0 { "Cannot move left" } else { "Cannot move right" },
			);
			return;
		};
		let names: Vec<&str> = order.iter().map(|&t| crate::settings::TAB_NAMES[t]).collect();
		for p in [pos, new_pos] {
			let unread_chats = crate::ui::message_display::unread_chats_in_tab(&state_nav_move.borrow(), order[p]);
			nav_list_move.set_string(p as u32, &crate::settings::nav_tab_label(order[p], unread_chats));
		}
		nav_list_move.set_selection(new_pos as u32, true);
		if let Some(menu_bar) = frame_nav_move.get_menu_bar() {
			reorder_go_menu(&menu_bar, &order);
		}
		{
			let mut s = state_nav_move.borrow_mut();
			s.settings.tab_order = order;
			s.settings.save();
		}
		let msg = match (new_pos.checked_sub(1), names.get(new_pos + 1)) {
			(Some(before), Some(after)) => format!("Moved between {} and {}", names[before], after),
			(None, Some(after)) => format!("Moved before {after}"),
			(Some(before), None) => format!("Moved after {}", names[before]),
			(None, None) => format!("Moved {}", names[new_pos]),
		};
		crate::platform::announce::announce(nav_move_announcer, &msg);
	});

	let tx_send_field = ui_tx.clone();
	let conv_msg_field_esc = parts.conversations_parts.message_field;
	let cancel_edit_btn_field = parts.conversations_parts.cancel_edit_btn;
	let state_field = state.clone();
	let conv_parts_field = parts.conversations_parts;
	conv_msg_field_esc.on_key_down(move |event| {
		if let WindowEventData::Keyboard(ref key_event) = event {
			let code = key_event.get_key_code();
			if crate::ui_commands::mentions::handle_key(
				&state_field,
				&conv_parts_field,
				code,
				key_event.shift_down(),
				key_event.control_down(),
			) {
				event.skip(false);
				return;
			}
			if code == Some(WXK_BACK)
				&& !key_event.control_down()
				&& crate::ui::emoji_replace::undo_on_backspace(&conv_msg_field_esc)
			{
				event.skip(false);
				return;
			}
			let is_editing = state_field.borrow().editing_message_id.is_some();
			if keys::is_enter(code) {
				let sends = state_field.borrow().settings.enter_is_send && !key_event.shift_down();
				if !sends {
					event.skip(true);
				} else {
					let text = conv_msg_field_esc.get_value();
					if is_editing {
						let message_id = state_field.borrow().editing_message_id.clone().unwrap_or_default();
						let _ = tx_send_field.send(UiCommand::EditMessage { message_id, new_text: text });
					} else if !text.is_empty() {
						let _ = tx_send_field.send(UiCommand::SendMessage(text));
					}
					event.skip(false);
				}
			} else if keys::is_escape(code) {
				if is_editing {
					let _ = tx_send_field.send(UiCommand::CancelEdit);
				} else if state_field.borrow().pending_reply_id.is_some() {
					let _ = tx_send_field.send(UiCommand::RemoveQuote);
				} else {
					let _ = tx_send_field.send(UiCommand::CloseConversation);
				}
				event.skip(false);
			} else if keys::is_tab(code) && is_editing && !key_event.shift_down() {
				cancel_edit_btn_field.set_focus();
				event.skip(false);
			} else if keys::is_letter(code, 'V') && key_event.control_down() {
				let mut has_files = false;
				if let Some(_locker) = Clipboard::get().locker() {
					let fdo = FileDataObject::new();
					if Clipboard::get().get_data(&fdo) && fdo.get_file_count() > 0 {
						has_files = true;
					}
				}
				if has_files {
					let _ = tx_send_field.send(UiCommand::PasteAttachment);
					event.skip(false);
				} else {
					event.skip(true);
				}
			} else {
				event.skip(true);
			}
		} else {
			event.skip(true);
		}
	});

	let state_mention_focus = state.clone();
	let conv_parts_focus = parts.conversations_parts;
	parts.conversations_parts.message_field.on_set_focus(move |event| {
		crate::ui_commands::mentions::refresh(&state_mention_focus, &conv_parts_focus);
		event.skip(true);
	});
	let state_mention_click = state.clone();
	let conv_parts_click = parts.conversations_parts;
	parts.conversations_parts.mention_list.on_selection_changed(move |event| {
		if let Some(idx) = event.get_selection() {
			crate::ui_commands::mentions::pick(&state_mention_click, &conv_parts_click, idx as usize);
		}
	});

	let tx_escape_hook = ui_tx.clone();
	let conv_panel_hook = parts.conversations_parts.conversation_panel;
	let search_panel_hook = parts.conversations_parts.search_panel;
	let state_hook = state.clone();
	let composer_hook = parts.conversations_parts.message_field;
	let chat_search_hook = parts.conversations_parts.search_field;
	let chat_list_hook = parts.conversations_parts.conversations_list;
	parts.frame.bind_internal(wxdragon::event::EventType::CHAR_HOOK, move |event| {
		let data = WindowEventData::new(event);
		if let WindowEventData::Keyboard(ref key_event) = data
			&& keys::is_escape(key_event.get_key_code())
		{
			if state_hook.borrow().mention_popup.is_some() && composer_hook.has_focus() {
				data.skip(true);
				return;
			}
			// Escape in the chat list's search box clears the search, keeps
			// the filter and takes the focus off the box, as in WA Web -
			// before anything below closes the open chat instead.
			if chat_search_hook.has_focus() {
				if !chat_search_hook.get_value().is_empty() {
					chat_search_hook.set_value("");
				}
				chat_list_hook.set_focus();
				data.skip(false);
				return;
			}
			if state_hook.borrow().chats_unlocked {
				let _ = tx_escape_hook.send(UiCommand::CloseLockedChats);
				data.skip(false);
				return;
			}
			// The search in the open chat closes first, back to its messages.
			if conv_panel_hook.is_shown() && search_panel_hook.is_shown() && !state_hook.borrow().selection_mode {
				let _ = tx_escape_hook.send(UiCommand::CloseSearch);
				data.skip(false);
				return;
			}
			if conv_panel_hook.is_shown() && !search_panel_hook.is_shown() && !state_hook.borrow().selection_mode {
				// This hook runs ahead of whichever control has focus, so
				// it's the one place Escape reliably reaches regardless
				// of where the user's focus is - including mid-edit,
				// which is why the composer's own on_key_down Escape
				// handling for editing never actually ran before: this
				// unconditional CloseConversation always won the race.
				if state_hook.borrow().is_voice_recording {
					let _ = tx_escape_hook.send(UiCommand::DiscardRecording);
				} else if state_hook.borrow().editing_message_id.is_some() {
					let _ = tx_escape_hook.send(UiCommand::CancelEdit);
				} else if state_hook.borrow().pending_reply_id.is_some() {
					let _ = tx_escape_hook.send(UiCommand::RemoveQuote);
				} else {
					let _ = tx_escape_hook.send(UiCommand::CloseConversation);
				}
				data.skip(false);
				return;
			}
		}
		data.skip(true);
	});

	// Chats and contacts are matched as each key is typed; message text is
	// searched once typing pauses, as WA Web debounces it (300 ms).
	let message_search_timer = Rc::new(Timer::new(&parts.frame));
	let tx_message_search = ui_tx.clone();
	message_search_timer.on_tick(move |_| {
		let _ = tx_message_search.send(UiCommand::SearchAllMessages);
	});
	let tx_filter = ui_tx.clone();
	let conv_search = parts.conversations_parts.search_field;
	let message_search_typing = message_search_timer.clone();
	conv_search.on_text_changed(move |_| {
		let query = conv_search.get_value();
		let _ = tx_filter.send(UiCommand::FilterConversations(query));
		message_search_typing.stop();
		message_search_typing.start(300, true);
	});

	// WA Web's search box keys: Enter opens the first result; Down (from the
	// end of the text) goes to the first result, skipping headings.
	let state_conv_search = state.clone();
	let tx_search_enter = ui_tx.clone();
	let conv_list_search = parts.conversations_parts.conversations_list;
	conv_search.on_key_down(move |event| {
		if let WindowEventData::Keyboard(ref key_event) = event {
			let code = key_event.get_key_code();
			let plain = !key_event.control_down() && !key_event.shift_down() && !key_event.alt_down();
			let (active_tab, first_result) = {
				let s = state_conv_search.borrow();
				(s.active_tab, s.filtered_chats.iter().position(|jid| !jid.is_empty()))
			};
			if keys::is_enter(code) && active_tab == 6 {
				let _ = tx_search_enter.send(UiCommand::PlaceCall);
				event.skip(false);
				return;
			}
			let chat_tab = !matches!(active_tab, 1 | 2 | 6);
			if let Some(first) = first_result.filter(|_| chat_tab && plain) {
				if keys::is_enter(code) && !conv_search.get_value().trim().is_empty() {
					let _ = tx_search_enter.send(UiCommand::OpenConversation(first));
					event.skip(false);
					return;
				}
				if keys::is_down(code)
					&& conv_search.get_insertion_point() >= conv_search.get_last_position()
					&& (first as u32) < conv_list_search.get_count()
				{
					conv_list_search.set_focus();
					conv_list_search.set_selection(first as u32, true);
					event.skip(false);
					return;
				}
			}
		}
		event.skip(true);
	});

	let tx_open = ui_tx.clone();
	parts.conversations_parts.conversations_list.on_item_double_clicked(move |event| {
		if let Some(idx) = event.get_selection() {
			let _ = tx_open.send(UiCommand::OpenConversation(idx as usize));
		}
	});

	let convs_list_ctx = parts.conversations_parts.conversations_list;
	let state_chat_ctx = state.clone();
	let ui_tx_chat_ctx = ui_tx.clone();
	let parent_frame_chat_ctx = parts.frame;

	let state_menu_open = state.clone();
	let frame_menu_open = parts.frame;
	parts.frame.on_menu_opened(move |_| {
		if let Ok(s) = state_menu_open.try_borrow() {
			refresh_menu_bar(&s, &frame_menu_open);
		}
	});

	let guard_menu_bar = enter_release_handled.clone();
	parts.frame.on_menu_selected(move |evt| {
		if (MENU_ID_SETTINGS..=6099).contains(&evt.get_id()) && enter_key_held() {
			guard_menu_bar.set(true);
		}
	});

	let guard_ctx = enter_release_handled.clone();
	let tx_status_menu = ui_tx.clone();
	let last_queued_menu =
		Rc::new(std::cell::Cell::new(std::time::Instant::now() - std::time::Duration::from_secs(10)));
	let open_chat_context_menu = Rc::new(move || {
		if let Some(idx) = convs_list_ctx.get_selection() {
			let queued = match state_chat_ctx.borrow().active_tab {
				1 => Some(UiCommand::ShowStatusMenu(idx as usize)),
				2 => Some(UiCommand::ShowStarredMenu(idx as usize)),
				6 => Some(UiCommand::ShowCallsMenu(idx as usize)),
				_ => None,
			};
			if let Some(cmd) = queued {
				if last_queued_menu.get().elapsed() > std::time::Duration::from_millis(700) {
					last_queued_menu.set(std::time::Instant::now());
					let _ = tx_status_menu.send(cmd);
				}
				return;
			}
			let pin_id = 4001;
			let archive_id = 4002;
			let lock_id = 4012;
			let mute_id = 4003;
			let mute_for_id = 4018;
			let read_id = 4004;
			let delete_id = 4005;
			let clear_id = 4013;
			let export_id = 4006;
			let export_text_id = 4007;
			let leave_group_id = 4008;
			let alert_silent_id = 4009;
			let alert_sound_id = 4010;
			let alert_full_id = 4011;
			let report_id = 4014;
			let block_id = 4015;
			let favorite_id = 4016;
			let lists_id = 4017;
			let contact_info_id = 4040;
			let notify_settings_id = 4041;
			let channel_info_id = 4042;

			let s = state_chat_ctx.borrow();
			// Headings, contacts and messages found by a search aren't chats,
			// so they have no chat menu.
			let is_message = s.filtered_message_ids.get(idx as usize).is_some_and(|id| !id.is_empty());
			let jid_opt = s.filtered_chats.get(idx as usize).filter(|j| !is_message && s.chats.contains(*j)).cloned();
			if let Some(jid) = jid_opt {
				let is_pinned = s.pinned_jids.contains(&jid);
				let is_archived = s.archived_jids.contains(&jid);
				let is_locked = s.locked_jids.contains(&jid);
				let is_muted = s.muted_jids.contains(&jid);
				let is_blocked = s.blocked_jids.contains(&jid);
				let is_favorite = s.favorite_jids.contains(&jid);
				let unread_count = s.unread_counts.get(&jid).copied().unwrap_or(0);
				let is_unread = unread_count > 0;
				let is_group = jid.ends_with("@g.us");
				let chat_name = s.contacts.get(&jid).cloned().unwrap_or_else(|| jid.clone());
				let archived_level = s.settings.archived_alert(&jid);
				drop(s);

				let mut menu_builder = Menu::builder();
				let is_person = jid.ends_with("@s.whatsapp.net") || jid.ends_with("@lid");
				let is_channel = jid.ends_with("@newsletter");
				if is_person {
					menu_builder = menu_builder.append_item(
						contact_info_id,
						"&Contact info",
						"See this contact's info, call them, or edit or delete the contact",
					);
				} else if is_channel {
					menu_builder = menu_builder.append_item(
						channel_info_id,
						"C&hannel info",
						"See this channel's details without opening it",
					);
				}

				let pin_label = if is_pinned { "Un&pin chat" } else { "&Pin chat" };
				let archive_label = if is_archived { "&Unarchive chat" } else { "&Archive chat" };
				let lock_label = if is_locked { "Un&lock chat" } else { "&Lock chat" };
				let read_label = if is_unread { "Mark as &read" } else { "Mark as &unread" };

				// WA Web has no Pin for an archived chat: unarchive it first.
				let show_pin = !is_archived || is_pinned;
				if show_pin {
					menu_builder = menu_builder.append_item(pin_id, pin_label, "Pin or unpin chat");
				}
				menu_builder = menu_builder
					.append_item(archive_id, archive_label, "Archive or unarchive chat")
					.append_item(lock_id, lock_label, "Lock or unlock chat")
					.append_item(read_id, read_label, "Mark chat as read or unread");
				if !jid.ends_with("@broadcast") {
					let favorite_label = if is_favorite { "Remove from fa&vorites" } else { "Add to fa&vorites" };
					menu_builder = menu_builder
						.append_item(favorite_id, favorite_label, "Show or stop showing this chat under Favorites")
						.append_item(lists_id, "Add to l&ist...", "Choose which of your lists this chat is in");
				}
				menu_builder = menu_builder
					.append_item(export_id, "&Export chat...", "Export this chat's transcript and media")
					.append_item(
						export_text_id,
						"Export &text only (Markdown)...",
						"Export just the message text as a Markdown file, no media",
					);
				if is_group {
					menu_builder = menu_builder.append_item(leave_group_id, "Leave &group...", "Leave this group");
				}
				menu_builder = menu_builder.append_item(
					clear_id,
					"Clea&r chat...",
					"Delete this chat's messages, keeping the chat itself",
				);
				if !jid.ends_with("@newsletter") && !jid.ends_with("@broadcast") {
					let what =
						if is_group { "Report this group to WhatsApp" } else { "Report this contact to WhatsApp" };
					menu_builder = menu_builder.append_item(report_id, "Re&port...", what);
				}
				if is_person {
					menu_builder = if is_blocked {
						menu_builder.append_item(
							block_id,
							"Un&block contact",
							"Let this contact message and call you again",
						)
					} else {
						menu_builder.append_item(
							block_id,
							"&Block contact...",
							"Stop this contact messaging and calling you",
						)
					};
				}
				menu_builder = menu_builder.append_item(delete_id, "&Delete chat...", "Delete chat history");

				let mut popup_menu = menu_builder.build();

				let mute_pos = usize::from(is_person || is_channel) + usize::from(show_pin) + 3;
				if is_muted || !crate::ui_commands::chat_list::has_mute_durations(&jid) {
					let label = if is_muted { "&Unmute notifications" } else { "&Mute notifications" };
					popup_menu.insert(mute_pos, mute_id, label, "Mute or unmute notifications", ItemKind::Normal);
				} else {
					let ui_tx_mute = ui_tx_chat_ctx.clone();
					let jid_mute = jid.clone();
					let mute_menu = crate::ui_commands::chat_list::mute_duration_menu(mute_for_id, move |seconds| {
						let _ =
							ui_tx_mute.send(UiCommand::ToggleMuteChat { jid: jid_mute.clone(), mute: true, seconds });
					});
					popup_menu.insert_submenu(
						mute_pos,
						mute_menu,
						"&Mute notifications",
						"Choose how long to mute this chat",
					);
				}
				if !jid.ends_with("@broadcast") {
					popup_menu.insert(
						mute_pos + 1,
						notify_settings_id,
						"Notification se&ttings...",
						"How this computer alerts you about this chat, without changing your phone",
						ItemKind::Normal,
					);
				}

				// Archived chats are silent by default; this submenu is where one
				// gets dialled back up. It has to be appended after `build()`
				// because only a finished `Menu` can take a submenu, which also
				// means it lands last - no bad thing, since it keeps "Delete
				// chat..." away from the bottom edge of the menu.
				//
				// A submenu item's event is offered to the submenu's own handler
				// and then to the invoking window, never to the parent menu's, so
				// this needs its own `on_selected` rather than riding on the one
				// below. The closure is boxed into the underlying wxMenu, so it
				// outlives `append_submenu` taking ownership of the wrapper.
				if is_archived {
					let alert_menu = Menu::builder()
						.append_radio_item(alert_silent_id, "&Silent", "No sound and no notification for this chat")
						.append_radio_item(
							alert_sound_id,
							"Sound &only",
							"Play the message sound but show no notification",
						)
						.append_radio_item(alert_full_id, "&Notify normally", "Alert exactly like an unarchived chat")
						.build();
					alert_menu.check_item(
						match archived_level {
							whatsnap::ArchivedAlert::Silent => alert_silent_id,
							whatsnap::ArchivedAlert::SoundOnly => alert_sound_id,
							whatsnap::ArchivedAlert::Full => alert_full_id,
						},
						true,
					);

					let ui_tx_alert = ui_tx_chat_ctx.clone();
					let jid_alert = jid.clone();
					alert_menu.on_selected(move |menu_evt| {
						let id = menu_evt.get_id();
						let level = if id == alert_silent_id {
							whatsnap::ArchivedAlert::Silent
						} else if id == alert_sound_id {
							whatsnap::ArchivedAlert::SoundOnly
						} else if id == alert_full_id {
							whatsnap::ArchivedAlert::Full
						} else {
							return;
						};
						let _ = ui_tx_alert.send(UiCommand::SetArchivedAlert { jid: jid_alert.clone(), level });
					});

					popup_menu.append_submenu(
						alert_menu,
						"Archived a&lerts",
						"How this archived chat alerts you about new messages",
					);
				}

				let ui_tx_clone = ui_tx_chat_ctx.clone();
				let jid_clone = jid.clone();
				let parent_clone = parent_frame_chat_ctx;

				popup_menu.on_selected(move |menu_evt| {
					let id = menu_evt.get_id();
					if id == contact_info_id {
						let _ = ui_tx_clone.send(UiCommand::ShowContactInfo(jid_clone.clone()));
					} else if id == channel_info_id {
						let _ = ui_tx_clone.send(UiCommand::ShowChannelInfo(jid_clone.clone()));
					} else if id == notify_settings_id {
						let _ = ui_tx_clone.send(UiCommand::ShowChatNotify { jid: jid_clone.clone() });
					} else if id == pin_id {
						let _ = ui_tx_clone.send(UiCommand::TogglePinChat { jid: jid_clone.clone(), pin: !is_pinned });
					} else if id == archive_id {
						let _ = ui_tx_clone
							.send(UiCommand::ToggleArchiveChat { jid: jid_clone.clone(), archive: !is_archived });
					} else if id == lock_id {
						let _ =
							ui_tx_clone.send(UiCommand::ToggleLockChat { jid: jid_clone.clone(), lock: !is_locked });
					} else if id == report_id {
						let _ = ui_tx_clone.send(UiCommand::ReportChat { jid: jid_clone.clone() });
					} else if id == favorite_id {
						let _ =
							ui_tx_clone.send(UiCommand::SetFavorite { jid: jid_clone.clone(), favorite: !is_favorite });
					} else if id == lists_id {
						let _ = ui_tx_clone.send(UiCommand::ChooseChatLists(jid_clone.clone()));
					} else if id == block_id {
						let _ = ui_tx_clone.send(UiCommand::ToggleBlockChat { jid: jid_clone.clone() });
					} else if id == mute_id {
						let _ = ui_tx_clone.send(UiCommand::ToggleMuteChat {
							jid: jid_clone.clone(),
							mute: !is_muted,
							seconds: 0,
						});
					} else if id == read_id {
						let _ = ui_tx_clone.send(UiCommand::ToggleReadChat { jid: jid_clone.clone(), read: is_unread });
					} else if id == delete_id {
						if is_group {
							let choices: Vec<&str> = vec!["Delete chat only", "Delete chat and exit group"];
							let dialog = SingleChoiceDialog::builder(
								&parent_clone,
								&format!("Delete chat with {}?", chat_name),
								"Confirm Delete",
								&choices,
							)
							.build();
							if dialog.show_modal() == wxdragon::id::ID_OK {
								match dialog.get_string_selection().as_deref() {
									Some("Delete chat only") => {
										let _ = ui_tx_clone.send(UiCommand::DeleteChat { jid: jid_clone.clone() });
									}
									Some("Delete chat and exit group") => {
										let _ = ui_tx_clone
											.send(UiCommand::DeleteChatAndLeaveGroup { jid: jid_clone.clone() });
									}
									_ => {}
								}
							}
						} else {
							let dialog = MessageDialog::builder(
								&parent_clone,
								&format!("Are you sure you want to delete chat with {}?", chat_name),
								"Confirm Delete",
							)
							.with_style(
								MessageDialogStyle::OK | MessageDialogStyle::Cancel | MessageDialogStyle::IconQuestion,
							)
							.build();
							if dialog.show_modal() == wxdragon::id::ID_OK {
								let _ = ui_tx_clone.send(UiCommand::DeleteChat { jid: jid_clone.clone() });
							}
						}
					} else if id == clear_id {
						let dialog = MessageDialog::builder(
							&parent_clone,
							&format!(
								"Delete all messages in the chat with {}? The chat itself will stay in your list.",
								chat_name
							),
							"Confirm Clear Chat",
						)
						.with_style(
							MessageDialogStyle::OK | MessageDialogStyle::Cancel | MessageDialogStyle::IconQuestion,
						)
						.build();
						if dialog.show_modal() == wxdragon::id::ID_OK {
							let _ = ui_tx_clone.send(UiCommand::ClearChat { jid: jid_clone.clone() });
						}
					} else if id == leave_group_id {
						let dialog = MessageDialog::builder(
							&parent_clone,
							&format!("Are you sure you want to leave {}?", chat_name),
							"Leave Group",
						)
						.with_style(
							MessageDialogStyle::OK | MessageDialogStyle::Cancel | MessageDialogStyle::IconQuestion,
						)
						.build();
						if dialog.show_modal() == wxdragon::id::ID_OK {
							let _ = ui_tx_clone.send(UiCommand::LeaveGroup { jid: jid_clone.clone() });
						}
					} else if id == export_id {
						let default_name = format!("{}.zip", sanitize_export_filename(&chat_name));
						let dialog = FileDialog::builder(&parent_clone)
							.with_message("Export chat as")
							.with_default_file(&default_name)
							.with_wildcard("Zip archive (*.zip)|*.zip")
							.with_style(FileDialogStyle::Save | FileDialogStyle::OverwritePrompt)
							.build();
						if dialog.show_modal() == wxdragon::id::ID_OK
							&& let Some(path) = dialog.get_path()
						{
							let _ = ui_tx_clone.send(UiCommand::ExportChat { jid: jid_clone.clone(), dest_path: path });
						}
					} else if id == export_text_id {
						let default_name = format!("{}.md", sanitize_export_filename(&chat_name));
						let dialog = FileDialog::builder(&parent_clone)
							.with_message("Export chat text as")
							.with_default_file(&default_name)
							.with_wildcard("Markdown file (*.md)|*.md")
							.with_style(FileDialogStyle::Save | FileDialogStyle::OverwritePrompt)
							.build();
						if dialog.show_modal() == wxdragon::id::ID_OK
							&& let Some(path) = dialog.get_path()
						{
							let _ =
								ui_tx_clone.send(UiCommand::ExportChatText { jid: jid_clone.clone(), dest_path: path });
						}
					}
				});

				convs_list_ctx.popup_menu(&mut popup_menu, None);
				popup_menu.destroy_menu();
				guard_ctx.set(true);
			}
		}
	});

	// Enter opens the chat. The guard is the app-wide one: closing a context
	// menu or a menu-bar item with Enter leaves its release behind on this
	// list, and that release mustn't open a chat.
	let conv_list_key = parts.conversations_parts.conversations_list;
	let conv_list_key_delete = parts.conversations_parts.conversations_list;
	let tx_open_enter = ui_tx.clone();
	keys::on_enter(&parts.conversations_parts.conversations_list, enter_release_handled.clone(), move || {
		if let Some(idx) = conv_list_key.get_selection() {
			let _ = tx_open_enter.send(UiCommand::OpenConversation(idx as usize));
		}
	});

	let open_chat_menu_key = open_chat_context_menu.clone();
	let state_conv_delete = state.clone();
	let tx_delete_call = ui_tx.clone();
	let frame_chat_list_keys = parts.frame;
	let chat_search_key = parts.conversations_parts.search_field;
	parts.conversations_parts.conversations_list.on_key_down(move |event| {
		if let WindowEventData::Keyboard(ref key_event) = event {
			// The menu bar's shortcut handles these, unless the open chat's
			// Conversation menu leaves the item out (a channel has no Lock,
			// for one); then the list sends it for the focused chat itself.
			let action = {
				let s = state_conv_delete.borrow();
				let menu_bar = frame_chat_list_keys.get_menu_bar();
				CHAT_LIST_ACTIONS
					.iter()
					.find(|(_, name)| s.keymap.is_pressed(name, key_event))
					.map(|(id, _)| *id)
					.filter(|id| menu_bar.as_ref().is_none_or(|bar| bar.find_item(*id).is_none()))
			};
			if let Some(id) = action {
				unsafe {
					wxdragon::ffi::wxd_Window_PostMenuCommand(
						frame_chat_list_keys.handle_ptr() as *mut wxdragon::ffi::wxd_Window_t,
						id,
					);
				}
				event.skip(false);
				return;
			}
			if keys::is_context_menu_key(key_event) {
				open_chat_menu_key.as_ref()();
				event.skip(false);
				return;
			}
			// Up from the first search result goes back to the search box.
			if keys::is_up(key_event.get_key_code()) {
				let s = state_conv_delete.borrow();
				let first = s.filtered_chats.iter().position(|jid| !jid.is_empty());
				if !matches!(s.active_tab, 1 | 2 | 6)
					&& !s.search_query.trim().is_empty()
					&& first.is_some_and(|first| conv_list_key_delete.get_selection() == Some(first as u32))
				{
					drop(s);
					chat_search_key.set_focus();
					chat_search_key.set_insertion_point_end();
					event.skip(false);
					return;
				}
			}
			if keys::is_delete(key_event.get_key_code())
				&& !key_event.control_down()
				&& !key_event.shift_down()
				&& !key_event.alt_down()
			{
				if state_conv_delete.borrow().active_tab == 6
					&& let Some(idx) = conv_list_key_delete.get_selection()
				{
					let _ = tx_delete_call.send(UiCommand::DeleteCall(idx as usize));
					event.skip(false);
					return;
				}
				if state_conv_delete.borrow().active_tab == 2
					&& let Some(idx) = conv_list_key_delete.get_selection()
				{
					let _ = tx_delete_call.send(UiCommand::UnstarStarredMessage(idx as usize));
					event.skip(false);
					return;
				}
			}
			if key_event.control_down()
				&& !key_event.shift_down()
				&& !key_event.alt_down()
				&& keys::is_letter(key_event.get_key_code(), 'C')
				&& state_conv_delete.borrow().active_tab == 2
				&& let Some(idx) = conv_list_key_delete.get_selection()
			{
				let _ = tx_delete_call.send(UiCommand::CopyStarredMessage(idx as usize));
				event.skip(false);
				return;
			}
		}
		event.skip(true);
	});

	let open_chat_menu_cb = open_chat_context_menu.clone();
	parts.conversations_parts.conversations_list.bind_internal(EventType::CONTEXT_MENU, move |_event| {
		open_chat_menu_cb.as_ref()();
	});

	let tx_play_media = ui_tx.clone();
	let msgs_list_play = parts.conversations_parts.messages_list;
	let state_play = state.clone();
	let play_voice_btn_space = parts.conversations_parts.play_voice_btn;
	let tx_close_list = ui_tx.clone();
	let parent_frame = parts.frame;
	let msgs_announcer = parts.conversations_parts.messages_announcer;
	let msg_input_field = parts.conversations_parts.message_field;
	let search_panel_key = parts.conversations_parts.search_panel;
	let conv_parts_play = parts.conversations_parts;
	let _conv_panel_key = parts.conversations_parts.conversation_panel;
	let last_left_arrow =
		std::rc::Rc::new(std::cell::Cell::new(std::time::Instant::now() - std::time::Duration::from_secs(10)));
	let msgs_list_ctx = parts.conversations_parts.messages_list;
	let state_ctx = state.clone();
	let parent_frame_ctx = parts.frame;
	let msgs_announcer_ctx = parts.conversations_parts.messages_announcer;
	let msg_input_field_ctx = parts.conversations_parts.message_field;
	let tx_play_media_ctx = ui_tx.clone();
	let conv_parts_ctx = parts.conversations_parts;

	let state_copy = state.clone();
	let tx_copy = ui_tx.clone();
	let msgs_announcer_copy = parts.conversations_parts.messages_announcer;
	let msgs_list_copy = parts.conversations_parts.messages_list;

	let perform_copy_selected = std::rc::Rc::new(move || {
		if let Some(idx) = msgs_list_copy.get_selection() {
			let s = state_copy.borrow();
			if let Some(display_item) = s.display_items.get(idx as usize).cloned() {
				drop(s);
				match display_item {
					whatsnap::DisplayItem::Single(msg) => {
						let is_voice = msg.is_audio
							|| msg.media_type == "voice"
							|| msg.media_type == "audio"
							|| msg.text.starts_with("[Voice Message]")
							|| msg.text.starts_with("[Audio]");
						let is_media = is_voice
							|| !msg.media_type.is_empty()
							|| msg.text.starts_with("[Image]")
							|| msg.text.starts_with("[Photo]")
							|| msg.text.starts_with("[Video]")
							|| msg.text.starts_with("[Document]")
							|| msg.text.starts_with("[Sticker]");

						if is_media {
							if msg.is_media_downloaded()
								&& !msg.media_path.is_empty()
								&& std::path::Path::new(&msg.media_path).exists()
							{
								let display_name = msg.display_filename();
								let temp_dir = std::env::temp_dir().join("whantv_clipboard");
								let _ = std::fs::create_dir_all(&temp_dir);
								let temp_path = temp_dir.join(&display_name);
								let path_to_copy = if std::fs::copy(&msg.media_path, &temp_path).is_ok() {
									temp_path.to_string_lossy().to_string()
								} else {
									msg.media_path.clone()
								};
								if crate::copy_files_to_clipboard(&[path_to_copy]) {
									let msg_kind = if is_voice { "voice message" } else { "file" };
									crate::platform::announce::announce(
										msgs_announcer_copy,
										&format!("Copied {} to clipboard.", msg_kind),
									);
								} else {
									crate::platform::announce::announce(
										msgs_announcer_copy,
										"Failed to copy file to clipboard.",
									);
								}
							} else {
								let _ =
									tx_copy.send(UiCommand::CopyAlbumToClipboard { message_ids: vec![msg.id.clone()] });
							}
						} else {
							if crate::copy_text(&msg.text) {
								crate::platform::announce::announce(
									msgs_announcer_copy,
									"Copied message text to clipboard.",
								);
							}
						}
					}
					whatsnap::DisplayItem::Album { messages, .. } => {
						let msg_ids: Vec<String> = messages.iter().map(|m| m.id.clone()).collect();
						if !msg_ids.is_empty() {
							let _ = tx_copy.send(UiCommand::CopyAlbumToClipboard { message_ids: msg_ids });
						}
					}
				}
			}
		}
	});

	let copy_for_ctx = perform_copy_selected.clone();
	let copy_for_key = perform_copy_selected.clone();

	let open_msg_context_menu = Rc::new(move || {
		if let Some(idx) = msgs_list_ctx.get_selection() {
			let reply_id = 3001;
			let edit_id = 3002;
			let delete_id = 3003;
			let info_id = 3004;
			let copy_all_id = 3005;
			let forward_id = 3006;
			let copy_id = 3007;
			let save_voice_id = 3008;
			let react_dialog_id = 3010;
			let retry_id = 3011;
			let star_id = 3012;
			let resend_id = 3013;
			let copy_number_id = 3014;
			let message_number_id = 3015;
			let report_id = 3016;
			let report_admins_id = 3017;

			let s = state_ctx.borrow();
			let chat_jid = s.selected_chat.clone().unwrap_or_default();
			let display_item = s.display_items.get(idx as usize).cloned();
			let is_group_chat = s.selected_chat.as_ref().map(|j| j.ends_with("@g.us")).unwrap_or(false);
			let is_channel_chat = s.selected_chat.as_ref().is_some_and(|j| j.ends_with("@newsletter"));
			let can_post_in_channel = is_channel_chat
				&& s.selected_chat
					.as_ref()
					.map(|jid| crate::ui_commands::links::can_post_in_channel(&s, jid))
					.unwrap_or(false);
			drop(s);

			let is_divider = match display_item.as_ref() {
				Some(whatsnap::DisplayItem::Single(m)) => m.is_date_divider || m.is_unread_divider,
				_ => false,
			};
			if is_divider || display_item.is_none() {
				return;
			}

			let is_deleted = display_item.as_ref().map(|d| d.is_deleted()).unwrap_or(false);
			let now =
				std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default().as_secs() as i64;
			let can_edit = display_item
				.as_ref()
				.map(|item| match item {
					whatsnap::DisplayItem::Single(m) => m.is_editable(now, is_channel_chat),
					whatsnap::DisplayItem::Album { .. } => false,
				})
				.unwrap_or(false);

			let reportable_id = display_item
				.as_ref()
				.and_then(|item| match item {
					whatsnap::DisplayItem::Single(m) => {
						(!m.from_me && !m.is_date_divider && !m.is_system() && !m.is_deleted && m.media_type != "call")
							.then(|| m.id.clone())
					}
					whatsnap::DisplayItem::Album { messages, .. } => {
						messages.first().filter(|m| !m.from_me).map(|m| m.id.clone())
					}
				})
				.filter(|_| !is_channel_chat);

			let is_starred = display_item
				.as_ref()
				.map(|item| match item {
					whatsnap::DisplayItem::Single(m) => m.starred && !m.is_date_divider && !m.is_deleted,
					whatsnap::DisplayItem::Album { messages, .. } => {
						!messages.is_empty() && messages.iter().all(|m| m.starred)
					}
				})
				.unwrap_or(false);

			let is_from_me = display_item
				.as_ref()
				.map(|item| match item {
					whatsnap::DisplayItem::Single(m) => m.from_me && !m.is_date_divider,
					whatsnap::DisplayItem::Album { messages, .. } => {
						messages.first().map(|m| m.from_me).unwrap_or(false)
					}
				})
				.unwrap_or(false);
			let is_photo_album = display_item
				.as_ref()
				.map(|item| match item {
					whatsnap::DisplayItem::Album { messages, .. } => messages.iter().all(|m| m.media_type == "image"),
					_ => false,
				})
				.unwrap_or(false);
			let is_voice_msg = display_item
				.as_ref()
				.map(|item| match item {
					whatsnap::DisplayItem::Single(m) => {
						m.is_audio
							|| m.media_type == "voice"
							|| m.media_type == "audio"
							|| m.text.starts_with("[Voice Message]")
							|| m.text.starts_with("[Audio]")
					}
					_ => false,
				})
				.unwrap_or(false);
			let can_retry = display_item
				.as_ref()
				.map(|item| match item {
					whatsnap::DisplayItem::Single(m) => {
						m.from_me && !m.is_date_divider && (m.status == "pending" || m.status == "failed")
					}
					whatsnap::DisplayItem::Album { messages, .. } => messages
						.first()
						.map(|m| m.from_me && (m.status == "pending" || m.status == "failed"))
						.unwrap_or(false),
				})
				.unwrap_or(false);

			let is_unreadable = display_item
				.as_ref()
				.map(|item| match item {
					whatsnap::DisplayItem::Single(m) => m.is_unreadable(),
					_ => false,
				})
				.unwrap_or(false);

			let phone_match = display_item.as_ref().and_then(|item| match item {
				whatsnap::DisplayItem::Single(m) => m.first_phone_number(),
				whatsnap::DisplayItem::Album { .. } => None,
			});

			let sender_jid = if is_group_chat {
				display_item.as_ref().and_then(|item| match item {
					whatsnap::DisplayItem::Single(m)
						if !m.is_date_divider && !m.from_me && !m.sender_jid.is_empty() =>
					{
						Some(m.sender_jid.clone())
					}
					whatsnap::DisplayItem::Album { messages, .. } => messages
						.first()
						.filter(|m| !m.from_me && !m.sender_jid.is_empty())
						.map(|m| m.sender_jid.clone()),
					_ => None,
				})
			} else {
				None
			};
			let sender_name = sender_jid.as_ref().map(|jid| {
				let msg_name = display_item
					.as_ref()
					.map(|item| match item {
						whatsnap::DisplayItem::Single(m) => m.sender_name.clone(),
						whatsnap::DisplayItem::Album { sender_name, .. } => sender_name.clone(),
					})
					.filter(|n| !n.is_empty() && n != jid);

				let s = state_ctx.borrow();
				msg_name
					.or_else(|| s.contacts.get(jid).cloned())
					.unwrap_or_else(|| crate::ui::contact_picker::display_name_for_jid(jid))
			});

			let mut menu_builder = Menu::builder();
			menu_builder = menu_builder.append_item(copy_id, "&Copy\tCtrl+C", "Copy message text or file to clipboard");
			let can_reply = !is_deleted && (!is_channel_chat || can_post_in_channel);
			if can_reply {
				menu_builder = menu_builder.append_item(reply_id, "&Reply", "Reply to message");
			}
			if !is_deleted {
				menu_builder =
					menu_builder.append_item(forward_id, "&Forward...", "Forward message to another contact");
				if can_edit {
					menu_builder = menu_builder.append_item(edit_id, "&Edit", "Edit message");
				}
			}
			let can_delete = !is_channel_chat || can_post_in_channel || is_from_me;
			if can_delete {
				menu_builder = menu_builder.append_item(delete_id, "&Delete", "Delete message");
			}
			if !is_deleted {
				menu_builder = menu_builder.append_item(react_dialog_id, "&React...", "React to message");
			}
			let can_star_msg = match display_item {
				Some(whatsnap::DisplayItem::Single(ref m)) => crate::ui_commands::selection::can_star(&chat_jid, m),
				Some(whatsnap::DisplayItem::Album { ref messages, .. }) => {
					!messages.is_empty() && messages.iter().all(|m| crate::ui_commands::selection::can_star(&chat_jid, m))
				}
				_ => false,
			};
			if can_star_msg {
				let is_album = matches!(display_item, Some(whatsnap::DisplayItem::Album { .. }));
				let star_label = match (is_album, is_starred) {
					(true, true) => "&Unstar all",
					(true, false) => "&Star all",
					(false, true) => "&Unstar",
					(false, false) => "&Star",
				};
				menu_builder = menu_builder.append_item(star_id, star_label, "Star or unstar this message");
			}
			if can_retry && !is_deleted {
				menu_builder = menu_builder.append_item(retry_id, "Retry &send", "Retry sending this message");
			}
			if is_unreadable && !is_deleted {
				menu_builder =
					menu_builder.append_item(resend_id, "Re&quest again", "Ask your phone to send this message again");
			}
			if is_voice_msg && !is_deleted {
				menu_builder =
					menu_builder.append_item(save_voice_id, "&Save voice message...", "Save voice message to device");
			}
			let is_poll_msg =
				matches!(display_item, Some(whatsnap::DisplayItem::Single(ref m)) if m.is_poll() && !m.is_deleted);
			if (is_from_me || is_poll_msg) && !is_deleted {
				menu_builder =
					menu_builder.append_item(info_id, "Message &Info", "View delivery info, or a poll's votes");
			}
			if is_photo_album && !is_deleted {
				menu_builder =
					menu_builder.append_item(copy_all_id, "Copy all p&hotos", "Copy all photos to clipboard");
			}
			if phone_match.is_some() && !is_deleted {
				menu_builder = menu_builder
					.append_item(copy_number_id, "Copy Nu&mber", "Copy phone number to clipboard")
					.append_item(message_number_id, "Message Num&ber", "Start a chat with this phone number");
			}
			if reportable_id.is_some() && !is_deleted {
				menu_builder = menu_builder.append_item(report_id, "Re&port...", "Report this message to WhatsApp");
				if is_group_chat {
					menu_builder = menu_builder.append_item(
						report_admins_id,
						"Report to group &admins",
						"Send this message to the group's admins to review",
					);
				}
			}
			let mut popup_menu = menu_builder.build();
			let extras_msg = crate::ui_commands::message_extras::append_message_actions(
				&popup_menu,
				&state_ctx,
				display_item.as_ref(),
			);

			let channel_post = match &display_item {
				Some(whatsnap::DisplayItem::Single(m)) if !m.is_date_divider && !m.is_deleted => {
					let s = state_ctx.borrow();
					s.selected_chat
						.clone()
						.filter(|jid| crate::ui_commands::links::can_post_in_channel(&s, jid))
						.map(|jid| (jid, m.clone()))
				}
				_ => None,
			};
			if let Some((channel_jid, post)) = channel_post {
				let submenu = crate::ui_commands::channel_admin::channel_post_menu(&state_ctx, &channel_jid, &post);
				popup_menu.append_submenu(submenu, "C&hannel post", "Pin this post, or see who reacted or voted");
			}

			// Same reasoning as the archived-alerts submenu on the chat-list
			// context menu: a submenu can only be appended to an already-built
			// `Menu`, and its items are only ever offered to its own
			// `on_selected`, never the parent's - so this needs its own.
			if let (Some(sender_jid), Some(sender_name)) = (sender_jid.clone(), sender_name.clone()) {
				let message_sender_id = 3101;
				let call_sender_id = 3102;

				let sender_menu = Menu::builder()
					.append_item(
						message_sender_id,
						&format!("&Message {}", sender_name),
						"Open a one-on-one chat with this message's sender",
					)
					.append_item(
						call_sender_id,
						&format!("&Call {}", sender_name),
						"Start a call with this message's sender",
					)
					.build();

				let tx_sender_menu = tx_play_media_ctx.clone();
				let sender_jid_menu = sender_jid.clone();
				sender_menu.on_selected(move |menu_evt| {
					let id = menu_evt.get_id();
					if id == message_sender_id {
						let _ = tx_sender_menu.send(UiCommand::OpenChatByJid(sender_jid_menu.clone()));
					} else if id == call_sender_id {
						let _ = tx_sender_menu.send(UiCommand::OpenChatByJid(sender_jid_menu.clone()));
						let _ = tx_sender_menu.send(UiCommand::PlaceCall);
					}
				});

				popup_menu.append_submenu(
					sender_menu,
					&format!("&User actions for {}", sender_name),
					"Actions for this message's sender",
				);
			}

			let state_clone = state_ctx.clone();
			let parent_frame_clone = parent_frame_ctx;
			let msgs_announcer_clone = msgs_announcer_ctx;
			let msg_input_field_clone = msg_input_field_ctx;
			let tx_play_media_clone = tx_play_media_ctx.clone();
			let phone_match_clone = phone_match.clone();
			let copy_for_menu = copy_for_ctx.clone();

			popup_menu.on_selected(move |menu_evt| {
				let id = menu_evt.get_id();
				if let Some(msg) = extras_msg.as_ref()
					&& crate::ui_commands::message_extras::handle_message_action(
						id,
						&state_clone,
						&parent_frame_clone,
						msg,
					) {
					return;
				}
				if id == reply_id {
					let mut s = state_clone.borrow_mut();
					let is_channel = s.selected_chat.as_ref().is_some_and(|j| j.ends_with("@newsletter"));
					if is_channel
						&& !s
							.selected_chat
							.as_ref()
							.map(|j| crate::ui_commands::links::can_post_in_channel(&s, j))
							.unwrap_or(false)
					{
						return;
					}
					let msg_opt = s.display_items.get(idx as usize).and_then(|item| match item {
						whatsnap::DisplayItem::Single(m) => Some(m.clone()),
						whatsnap::DisplayItem::Album { messages, .. } => messages.first().cloned(),
					});
					if let Some(msg) = msg_opt
						&& !msg.is_date_divider
						&& !msg.is_deleted
					{
						let sender = if msg.from_me {
							"You".to_string()
						} else if !msg.sender_name.is_empty() {
							msg.sender_name.clone()
						} else {
							msg.sender_jid.clone()
						};
						let preview = if msg.is_audio || msg.text.starts_with("[Voice Message]") {
							"Voice message".to_string()
						} else if msg.text.is_empty() {
							"Media".to_string()
						} else {
							crate::ui::chat_ui::reply_snippet(&msg.text)
						};
						s.pending_reply_id = Some(msg.id.clone());
						s.pending_reply_sender =
							Some(if msg.from_me { "me".to_string() } else { msg.sender_jid.clone() });
						s.pending_reply_preview = Some(format!("Replying to {}: {}", sender, preview));

						let status_msg =
							format!("Replying to {}: {}. Type your reply and press Enter.", sender, preview);
						crate::platform::announce::announce(msgs_announcer_clone, &status_msg);
						crate::ui::chat_ui::update_composer_buttons(&s, &conv_parts_ctx);
						msg_input_field_clone.set_focus();
					}
				} else if id == forward_id {
					let s = state_clone.borrow();
					let msg_opt = s.display_items.get(idx as usize).and_then(|item| match item {
						whatsnap::DisplayItem::Single(m) => Some(m.clone()),
						whatsnap::DisplayItem::Album { messages, .. } => messages.first().cloned(),
					});
					if let Some(msg) = msg_opt
						&& !msg.is_date_divider
						&& !msg.is_deleted
					{
						let mut labels = Vec::new();
						let mut jids = Vec::new();
						for jid in &s.chats {
							if !jid.ends_with("@broadcast") && crate::ui_commands::links::can_forward_to(&s, jid) {
								let name = s.contacts.get(jid).cloned().unwrap_or_else(|| jid.clone());
								labels.push(name);
								jids.push(jid.clone());
							}
						}
						for (jid, name) in &s.contacts {
							if !jid.ends_with("@broadcast")
								&& !jids.contains(jid)
								&& crate::ui_commands::links::can_forward_to(&s, jid)
							{
								labels.push(name.clone());
								jids.push(jid.clone());
							}
						}
						drop(s);

						if !jids.is_empty() {
							let dlg = Dialog::builder(&parent_frame_clone, "Forward Message")
								.with_size(400, 450)
								.with_style(DialogStyle::DefaultDialogStyle | DialogStyle::ResizeBorder)
								.build();
							let dlg_panel = Panel::builder(&dlg).build();
							let dlg_sizer = BoxSizer::builder(Orientation::Vertical).build();

							let search_label =
								StaticText::builder(&dlg_panel).with_label("Search contacts or chats:").build();
							dlg_sizer.add(&search_label, 0, SizerFlag::Left | SizerFlag::Top | SizerFlag::Right, 8);

							let search_ctrl = TextCtrl::builder(&dlg_panel).build();
							dlg_sizer.add(&search_ctrl, 0, SizerFlag::Expand | SizerFlag::All, 8);

							let contacts_listbox = ListBox::builder(&dlg_panel).with_choices(labels.clone()).build();
							dlg_sizer.add(
								&contacts_listbox,
								1,
								SizerFlag::Expand | SizerFlag::Left | SizerFlag::Right,
								8,
							);

							let btn_panel = Panel::builder(&dlg_panel).build();
							let btn_sizer = BoxSizer::builder(Orientation::Horizontal).build();
							let ok_btn =
								Button::builder(&btn_panel).with_id(wxdragon::id::ID_OK).with_label("Forward").build();
							let cancel_btn = Button::builder(&btn_panel)
								.with_id(wxdragon::id::ID_CANCEL)
								.with_label("Cancel")
								.build();
							btn_sizer.add(&ok_btn, 0, SizerFlag::Right, 5);
							btn_sizer.add(&cancel_btn, 0, SizerFlag::empty(), 0);
							btn_panel.set_sizer(btn_sizer, true);

							dlg_sizer.add(&btn_panel, 0, SizerFlag::AlignRight | SizerFlag::All, 8);
							dlg_panel.set_sizer(dlg_sizer, true);

							let dlg_esc_search = dlg;
							search_ctrl.on_key_down(keys::escape_cancels(dlg_esc_search));

							let dlg_esc_list = dlg;
							contacts_listbox.on_key_down(keys::escape_cancels(dlg_esc_list));

							let current_labels = std::rc::Rc::new(std::cell::RefCell::new(labels.clone()));
							let current_jids = std::rc::Rc::new(std::cell::RefCell::new(jids.clone()));

							let all_labels = labels;
							let all_jids = jids;
							let current_labels_search = current_labels.clone();
							let current_jids_search = current_jids.clone();
							let listbox_search = contacts_listbox;

							search_ctrl.on_text_changed(move |_| {
								let q = search_ctrl.get_value().to_lowercase().trim().to_string();
								let mut fl = Vec::new();
								let mut fj = Vec::new();
								for (lbl, jid) in all_labels.iter().zip(all_jids.iter()) {
									if q.is_empty() || lbl.to_lowercase().contains(&q) {
										fl.push(lbl.clone());
										fj.push(jid.clone());
									}
								}
								listbox_search.clear();
								for l in &fl {
									listbox_search.append(l);
								}
								*current_labels_search.borrow_mut() = fl;
								*current_jids_search.borrow_mut() = fj;
							});

							let dlg_cancel = dlg;
							cancel_btn.on_click(move |_| {
								dlg_cancel.end_modal(0);
							});

							let dlg_ok = dlg;
							let tx_fwd = tx_play_media_clone.clone();
							let msg_id = msg.id.clone();
							let current_jids_ok = current_jids;
							ok_btn.on_click(move |_| {
								if let Some(sel_idx) = contacts_listbox.get_selection() {
									let cj = current_jids_ok.borrow();
									if (sel_idx as usize) < cj.len() {
										let target_jid = cj[sel_idx as usize].clone();
										dlg_ok.end_modal(wxdragon::id::ID_OK);
										let _ = tx_fwd.send(UiCommand::ForwardMessage {
											message_id: msg_id.clone(),
											to_jids: vec![target_jid],
										});
									}
								}
							});

							search_ctrl.set_focus();
							dlg.show_modal();
							// The base Dialog has no Drop, so a dialog built per use must be
							// destroyed or it (and every closure bound to it) lives on hidden.
							dlg.destroy();
						}
					}
				} else if id == edit_id {
					let s = state_clone.borrow();
					let msg_opt = s.display_items.get(idx as usize).and_then(|item| match item {
						whatsnap::DisplayItem::Single(m) => Some(m.clone()),
						whatsnap::DisplayItem::Album { messages, .. } => messages.first().cloned(),
					});
					drop(s);
					if let Some(msg) = msg_opt
						&& !msg.is_date_divider
					{
						let now = std::time::SystemTime::now()
							.duration_since(std::time::UNIX_EPOCH)
							.unwrap_or_default()
							.as_secs() as i64;
						let in_channel =
							state_clone.borrow().selected_chat.as_ref().is_some_and(|j| j.ends_with("@newsletter"));
						if !msg.is_editable(now, in_channel) {
							unsafe {
								let _ = windows::Win32::System::Diagnostics::Debug::MessageBeep(
									windows::Win32::UI::WindowsAndMessaging::MB_ICONHAND,
								);
							}
							let still_eligible_otherwise =
								msg.from_me && !msg.is_deleted && !msg.is_voice() && msg.edit_text().is_some();
							let reason = if still_eligible_otherwise {
								if in_channel {
									"This message can no longer be edited - the 30 day edit window has passed."
								} else {
									"This message can no longer be edited - the 15 minute edit window has passed."
								}
							} else {
								"Message can't be edited."
							};
							crate::platform::announce::announce(msgs_announcer_clone, reason);
						} else {
							let _ = tx_play_media_clone.send(UiCommand::BeginEditMessage {
								message_id: msg.id.clone(),
								text: msg.edit_text().unwrap_or_default().to_string(),
							});
						}
					}
				} else if id == delete_id {
					let ids = deletable_ids(&state_clone.borrow(), idx);
					if !ids.is_empty() {
						let _ = tx_play_media_clone.send(UiCommand::AskDeleteMessages(ids));
					}
				} else if id == star_id {
					let s = state_clone.borrow();
					let chat_jid = s.selected_chat.clone().unwrap_or_default();
					let item_opt = s.display_items.get(idx as usize).cloned();
					drop(s);
					if let Some(item) = item_opt {
						match item {
							whatsnap::DisplayItem::Single(msg) => {
								if !msg.is_date_divider && crate::ui_commands::selection::can_star(&chat_jid, &msg) {
									let sender_jid =
										if msg.from_me { "me".to_string() } else { msg.sender_jid.clone() };
									let _ = tx_play_media_clone.send(UiCommand::StarMessage {
										message_id: msg.id.clone(),
										sender_jid,
										from_me: msg.from_me,
										starred: !msg.starred,
									});
								}
							}
							whatsnap::DisplayItem::Album { messages, .. } => {
								let all_starred = !messages.is_empty() && messages.iter().all(|m| m.starred);
								let target_starred = !all_starred;
								for m in messages.iter().filter(|m| {
									m.starred != target_starred && crate::ui_commands::selection::can_star(&chat_jid, m)
								}) {
									let sender_jid = if m.from_me { "me".to_string() } else { m.sender_jid.clone() };
									let _ = tx_play_media_clone.send(UiCommand::StarMessage {
										message_id: m.id.clone(),
										sender_jid,
										from_me: m.from_me,
										starred: target_starred,
									});
								}
							}
						}
					}
				} else if id == report_id || id == report_admins_id {
					if let Some(message_id) = reportable_id.clone() {
						let cmd = if id == report_id {
							UiCommand::ReportMessage { message_id }
						} else {
							UiCommand::ReportToAdmins { message_id }
						};
						let _ = tx_play_media_clone.send(cmd);
					}
				} else if id == info_id {
					let s = state_clone.borrow();
					let msg_opt = s.display_items.get(idx as usize).and_then(|item| match item {
						whatsnap::DisplayItem::Single(m) => Some(m.clone()),
						whatsnap::DisplayItem::Album { messages, .. } => messages.first().cloned(),
					});
					if let Some(msg) = msg_opt
						&& (msg.from_me || msg.is_poll())
						&& !msg.is_date_divider
					{
						let _ = tx_play_media_clone.send(UiCommand::GetMessageInfo { message_id: msg.id.clone() });
					}
				} else if id == copy_id {
					copy_for_menu.as_ref()();
				} else if id == retry_id {
					let s = state_clone.borrow();
					let msg_opt = s.display_items.get(idx as usize).and_then(|item| match item {
						whatsnap::DisplayItem::Single(m) => Some(m.clone()),
						whatsnap::DisplayItem::Album { messages, .. } => messages.first().cloned(),
					});
					drop(s);
					if let Some(msg) = msg_opt {
						let _ = tx_play_media_clone.send(UiCommand::RetryMessage { message_id: msg.id.clone() });
					}
				} else if id == resend_id {
					let s = state_clone.borrow();
					let msg_opt = s.display_items.get(idx as usize).and_then(|item| match item {
						whatsnap::DisplayItem::Single(m) => Some(m.clone()),
						_ => None,
					});
					drop(s);
					if let Some(msg) = msg_opt {
						let _ =
							tx_play_media_clone.send(UiCommand::RequestMessageResend { message_id: msg.id.clone() });
					}
				} else if id == save_voice_id {
					let _ = tx_play_media_clone.send(UiCommand::SaveMediaAs);
				} else if id == copy_all_id {
					let s = state_clone.borrow();
					if let Some(whatsnap::DisplayItem::Album { messages, .. }) = s.display_items.get(idx as usize) {
						let msg_ids: Vec<String> = messages.iter().map(|m| m.id.clone()).collect();
						if !msg_ids.is_empty() {
							let _ = tx_play_media_clone.send(UiCommand::CopyAlbumToClipboard { message_ids: msg_ids });
						}
					}
				} else if id == copy_number_id {
					if let Some((_, digits)) = phone_match_clone.as_ref()
						&& crate::copy_text(&format!("+{}", digits))
					{
						crate::platform::announce::announce(msgs_announcer_clone, "Copied phone number to clipboard.");
					}
				} else if id == message_number_id {
					if let Some((_, digits)) = phone_match_clone.as_ref() {
						let _ = tx_play_media_clone.send(UiCommand::OpenChatByPhone(digits.clone()));
					}
				} else if id == react_dialog_id {
					let s = state_clone.borrow();
					let msg_opt = s.display_items.get(idx as usize).and_then(|item| match item {
						whatsnap::DisplayItem::Single(m) => Some(m.clone()),
						whatsnap::DisplayItem::Album { messages, .. } => messages.first().cloned(),
					});
					drop(s);
					if let Some(msg) = msg_opt {
						let dlg = Dialog::builder(&parent_frame_clone, "React to message").with_size(300, 400).build();
						let dlg_panel = Panel::builder(&dlg).build();
						let dlg_sizer = BoxSizer::builder(Orientation::Vertical).build();

						let hint_label = StaticText::builder(&dlg_panel).with_label("Select a reaction emoji:").build();
						dlg_sizer.add(&hint_label, 0, SizerFlag::All, 8);

						let emojis = vec![
							"👍".to_string(),
							"❤️".to_string(),
							"😂".to_string(),
							"😮".to_string(),
							"😢".to_string(),
							"🙏".to_string(),
							"🔥".to_string(),
							"🎉".to_string(),
							"💯".to_string(),
							"😎".to_string(),
							"🥰".to_string(),
							"Other emoji...".to_string(),
							"Remove reaction".to_string(),
						];

						let emoji_list = ListBox::builder(&dlg_panel).with_choices(emojis.clone()).build();
						dlg_sizer.add(&emoji_list, 1, SizerFlag::Expand | SizerFlag::Left | SizerFlag::Right, 8);

						dlg_panel.set_sizer(dlg_sizer, true);

						let dlg_ref = dlg;
						let tx_react = tx_play_media_clone.clone();
						let msg_id = msg.id.clone();
						let sender_jid = if msg.from_me { "me".to_string() } else { msg.sender_jid.clone() };
						let parent_ref = parent_frame_clone;

						let on_select = Rc::new(move |choice_idx: usize| {
							dlg_ref.end_modal(wxdragon::id::ID_OK);
							match choice_idx {
								0..=10 => {
									let selected = emojis[choice_idx].clone();
									let _ = tx_react.send(UiCommand::ReactToMessage {
										message_id: msg_id.clone(),
										sender_jid: sender_jid.clone(),
										emoji: selected,
									});
								}
								11 => {
									let custom_dlg = TextEntryDialog::builder(
										&parent_ref,
										"Enter or paste an emoji:",
										"React with Emoji",
									)
									.build();
									if custom_dlg.show_modal() == wxdragon::id::ID_OK
										&& let Some(val) = custom_dlg.get_value()
									{
										let emoji = val.trim().to_string();
										if !emoji.is_empty() {
											let _ = tx_react.send(UiCommand::ReactToMessage {
												message_id: msg_id.clone(),
												sender_jid: sender_jid.clone(),
												emoji,
											});
										}
									}
								}
								12 => {
									let _ = tx_react.send(UiCommand::ReactToMessage {
										message_id: msg_id.clone(),
										sender_jid: sender_jid.clone(),
										emoji: String::new(),
									});
								}
								_ => {}
							}
						});

						let on_select_dbl = on_select.clone();
						emoji_list.on_item_double_clicked(move |event| {
							if let Some(sel) = event.get_selection() {
								on_select_dbl(sel as usize);
							}
						});

						let on_select_key = on_select.clone();
						let list_ref = emoji_list;
						keys::on_enter(&emoji_list, Rc::default(), move || {
							if let Some(sel) = list_ref.get_selection() {
								on_select_key(sel as usize);
							}
						});
						emoji_list.on_key_down(keys::escape_cancels(dlg));

						emoji_list.set_focus();
						dlg.show_modal();
						// The base Dialog has no Drop, so a dialog built per use must be
						// destroyed or it (and every closure bound to it) lives on hidden.
						dlg.destroy();
					}
				}
			});

			msgs_list_ctx.popup_menu(&mut popup_menu, None);
			popup_menu.destroy_menu();
			// An item picked with Enter leaves its release for the message
			// list, which would open the message's link.
			if enter_key_held() {
				crate::platform::tray::enter_release_guard().set(true);
			}
		}
	});

	let open_menu_key = open_msg_context_menu.clone();
	let enter_guard_msgs_down = crate::platform::tray::enter_release_guard();
	parts.conversations_parts.messages_list.on_key_down(move |event| {
		if let WindowEventData::Keyboard(ref key_event) = event {
			let code = key_event.get_key_code();
			enter_guard_msgs_down.set(false);
			let (keymap, type_to_compose) = {
				let s = state_play.borrow();
				(s.keymap.clone(), s.settings.type_to_compose)
			};
			let pressed = |id| keymap.is_pressed_in_list(id, key_event, type_to_compose);

			if keys::is_left(code) {
				let now = std::time::Instant::now();
				let prev = last_left_arrow.get();
				if now.duration_since(prev) < std::time::Duration::from_millis(600) {
					last_left_arrow.set(now - std::time::Duration::from_secs(10));
					if search_panel_key.is_shown() {
						if let Some(idx) = msgs_list_play.get_selection() {
							let s = state_play.borrow();
							let msg_opt = s.display_items.get(idx as usize).and_then(|item| match item {
								whatsnap::DisplayItem::Single(m) => Some(m.clone()),
								whatsnap::DisplayItem::Album { messages, .. } => messages.first().cloned(),
							});
							if let Some(msg) = msg_opt {
								drop(s);
								if !msg.is_date_divider && (!msg.quoted_id.is_empty() || !msg.quoted_text.is_empty()) {
									let _ = tx_close_list.send(UiCommand::JumpToSearchResult {
										message_id: msg.quoted_id.clone(),
										message_text: msg.quoted_text.clone(),
									});
								} else {
									let _ = tx_close_list.send(UiCommand::CloseSearch);
								}
							} else {
								drop(s);
								let _ = tx_close_list.send(UiCommand::CloseSearch);
							}
						} else {
							let _ = tx_close_list.send(UiCommand::CloseSearch);
						}
						event.skip(false);
					} else if let Some(idx) = msgs_list_play.get_selection() {
						let s = state_play.borrow();
						let msg_opt = s.display_items.get(idx as usize).and_then(|item| match item {
							whatsnap::DisplayItem::Single(m) => Some(m.clone()),
							whatsnap::DisplayItem::Album { messages, .. } => messages.first().cloned(),
						});
						if let Some(msg) = msg_opt
							&& (!msg.quoted_id.is_empty() || !msg.quoted_text.is_empty())
						{
							let mut target = if !msg.quoted_id.is_empty() {
								s.display_items.iter().position(|item| match item {
									whatsnap::DisplayItem::Single(m) => m.id == msg.quoted_id,
									whatsnap::DisplayItem::Album { messages, .. } => {
										messages.iter().any(|m| m.id == msg.quoted_id)
									}
								})
							} else {
								None
							};

							if target.is_none() && !msg.quoted_text.is_empty() {
								let is_generic = msg.quoted_text == "[Voice Message]"
									|| msg.quoted_text == "[Audio]"
									|| msg.quoted_text == "Voice message"
									|| msg.quoted_text == "Audio"
									|| msg.quoted_text == "[Media]";
								if !is_generic && (idx as usize) < s.display_items.len() {
									target = s.display_items[..idx as usize].iter().rposition(|item| match item {
										whatsnap::DisplayItem::Single(m) => {
											m.text == msg.quoted_text
												|| (!m.text.is_empty() && m.text.contains(&msg.quoted_text))
										}
										whatsnap::DisplayItem::Album { messages, .. } => messages.iter().any(|m| {
											m.text == msg.quoted_text
												|| (!m.text.is_empty() && m.text.contains(&msg.quoted_text))
										}),
									});
								}
							}

							if let Some(target_idx) = target {
								msgs_list_play.set_selection(target_idx as u32, true);
								unsafe {
									let _ = windows::Win32::System::Diagnostics::Debug::MessageBeep(
										windows::Win32::UI::WindowsAndMessaging::MB_OK,
									);
								}
							} else if !msg.quoted_id.is_empty() {
								let _ = tx_close_list.send(UiCommand::JumpToSearchResult {
									message_id: msg.quoted_id.clone(),
									message_text: String::new(),
								});
							} else {
								unsafe {
									let _ = windows::Win32::System::Diagnostics::Debug::MessageBeep(
										windows::Win32::UI::WindowsAndMessaging::MB_ICONASTERISK,
									);
								}
							}
						}
					}
					event.skip(false);
				} else {
					last_left_arrow.set(now);
					if crate::ui_commands::voice::is_voice_playing(&state_play.borrow()) {
						let _ = tx_close_list.send(UiCommand::SeekAudio(-5.0));
					} else if let Some(idx) = msgs_list_play.get_selection() {
						let said = state_play.borrow().display_items.get(idx as usize).and_then(|item| match item {
							whatsnap::DisplayItem::Single(m) => m.reply_announcement(),
							whatsnap::DisplayItem::Album { messages, .. } => {
								messages.first().and_then(|m| m.reply_announcement())
							}
						});
						if let Some(said) = said {
							crate::platform::announce::announce(msgs_announcer, &said);
						}
					}
					event.skip(false);
				}
			} else if keys::is_right(code) {
				if crate::ui_commands::voice::is_voice_playing(&state_play.borrow()) {
					let _ = tx_close_list.send(UiCommand::SeekAudio(5.0));
					event.skip(false);
				} else {
					event.skip(true);
				}
			} else if pressed(message_list::SEEK_BACK) {
				if crate::ui_commands::voice::is_voice_playing(&state_play.borrow()) {
					let _ = tx_close_list.send(UiCommand::SeekAudio(-5.0));
					event.skip(false);
				} else {
					event.skip(true);
				}
			} else if pressed(message_list::SEEK_FORWARD) {
				if crate::ui_commands::voice::is_voice_playing(&state_play.borrow()) {
					let _ = tx_close_list.send(UiCommand::SeekAudio(5.0));
					event.skip(false);
				} else {
					event.skip(true);
				}
			} else if pressed(message_list::STOP_AUDIO) {
				let stopping = state_play.borrow().playing_msg_id.is_some();
				if stopping {
					let _ = tx_close_list.send(UiCommand::StopVoiceMessage);
				}
				event.skip(!stopping);
			} else if pressed(message_list::SPEED_DOWN) {
				if crate::ui_commands::voice::is_voice_playing(&state_play.borrow()) {
					let _ = tx_close_list.send(UiCommand::DecreaseAudioSpeed);
					event.skip(false);
				} else {
					event.skip(true);
				}
			} else if pressed(message_list::SPEED_UP) {
				if crate::ui_commands::voice::is_voice_playing(&state_play.borrow()) {
					let _ = tx_close_list.send(UiCommand::IncreaseAudioSpeed);
					event.skip(false);
				} else {
					event.skip(true);
				}
			} else if keys::is_escape(key_event.get_key_code()) {
				let _ = tx_close_list.send(UiCommand::CloseConversation);
				event.skip(false);
			} else if keys::is_home(key_event.get_key_code()) {
				// WXK_HOME (313) -> Oldest message at top. set_selection() is a
				// programmatic call - unlike the real keyboard-driven navigation
				// Up/Down arrows go through, wxWidgets never fires
				// on_selection_changed for it, so the idx==0 check that triggers
				// LoadMoreMessages there never ran for Home on its own (only after
				// a follow-up Down+Up, which IS native navigation). Send it here
				// directly instead of relying on that handler for this case.
				//
				// From a window that doesn't reach the chat's start, load its
				// oldest page instead (and land on its first row).
				let count = msgs_list_play.get_count();
				if state_play.borrow().older_unloaded > 0 {
					let _ = tx_close_list.send(UiCommand::JumpToOldestMessage);
				} else if count > 0 {
					msgs_list_play.set_selection(0, true);
					let _ = tx_close_list.send(UiCommand::LoadMoreMessages);
				}
				event.skip(false);
			} else if keys::is_end(key_event.get_key_code()) {
				let count = msgs_list_play.get_count();
				if count > 0 {
					msgs_list_play.set_selection(count - 1, true);
				}
				let _ = tx_close_list.send(UiCommand::JumpToLatestMessage);
				event.skip(false);
			} else if keys::is_letter(code, 'C') && key_event.control_down() {
				copy_for_key.as_ref()();
				event.skip(false);
			} else if keys::is_letter(key_event.get_key_code(), 'V') && key_event.control_down() {
				let mut has_files = false;
				if let Some(_locker) = Clipboard::get().locker() {
					let fdo = FileDataObject::new();
					if Clipboard::get().get_data(&fdo) && fdo.get_file_count() > 0 {
						has_files = true;
					}
				}
				if has_files {
					let _ = tx_close_list.send(UiCommand::PasteAttachment);
					event.skip(false);
				} else {
					event.skip(true);
				}
			} else if key_event.get_key_code() == Some(WXK_RETURN) && key_event.shift_down() {
				event.skip(false);
			} else if key_event.get_key_code() == Some(WXK_RETURN) {
				let mut handled = false;
				if let Some(idx) = msgs_list_play.get_selection() {
					let mut s = state_play.borrow_mut();
					let msg_opt = s.display_items.get(idx as usize).and_then(|item| match item {
						whatsnap::DisplayItem::Single(m) => Some(m.clone()),
						whatsnap::DisplayItem::Album { messages, .. } => messages.first().cloned(),
					});
					if let Some(msg) = msg_opt {
						if msg.is_call() {
							handled = true;
							let cmd = match crate::ui_commands::calls::joinable_call_for_row(&s, &msg) {
								Some(call_id) => UiCommand::JoinGroupCall(Some(call_id)),
								None => UiCommand::PlaceCall,
							};
							let _ = tx_enter_call.send(cmd);
						}
						let is_voice =
							msg.is_audio || msg.text.starts_with("[Voice Message]") || msg.text.starts_with("[Audio]");
						if !handled && is_voice && !msg.is_deleted {
							handled = true;
							let is_currently_playing = s.playing_msg_id.as_deref() == Some(&msg.id);
							if is_currently_playing {
								if s.is_audio_paused {
									if let Some(ref sink) = s.audio_sink {
										sink.play();
									}
									s.is_audio_paused = false;
									s.audio_start_instant = Some(std::time::Instant::now());
									play_voice_btn_space.set_label("Pause voice message");
								} else {
									if let Some(inst) = s.audio_start_instant.take() {
										s.audio_elapsed_secs += inst.elapsed().as_secs_f32() * s.audio_speed;
									}
									if let Some(ref sink) = s.audio_sink {
										sink.pause();
									}
									s.is_audio_paused = true;
									play_voice_btn_space.set_label("Play voice message");
								}
							} else {
								if let Some(ref sink) = s.audio_sink {
									sink.stop();
								}
								s.audio_sink = None;
								s.audio_elapsed_secs = 0.0;
								s.audio_start_instant = None;
								s.is_audio_paused = false;
								s.playing_msg_id = Some(msg.id.clone());
								play_voice_btn_space.set_label("Loading voice message...");
								let _ = tx_play_media.send(UiCommand::PlayVoiceMessage(msg.id.clone()));
							}
						}
					}
				}
				event.skip(!handled);
			} else if pressed(message_list::SELECTION_MODE) {
				let _ = tx_close_list.send(UiCommand::ToggleSelectionMode);
				event.skip(false);
			} else if keys::is_space(key_event.get_key_code()) && !key_event.control_down() {
				if let Some(idx) = msgs_list_play.get_selection() {
					let mut s = state_play.borrow_mut();
					let msg_opt = s.display_items.get(idx as usize).and_then(|item| match item {
						whatsnap::DisplayItem::Single(m) => Some(m.clone()),
						whatsnap::DisplayItem::Album { messages, .. } => messages.first().cloned(),
					});
					if let Some(msg) = msg_opt {
						let is_voice =
							msg.is_audio || msg.text.starts_with("[Voice Message]") || msg.text.starts_with("[Audio]");
						if !is_voice || msg.is_deleted {
							event.skip(true);
							return;
						}
						let is_currently_playing = s.playing_msg_id.as_deref() == Some(&msg.id);
						if is_currently_playing {
							if s.is_audio_paused {
								if let Some(ref sink) = s.audio_sink {
									sink.play();
								}
								s.is_audio_paused = false;
								s.audio_start_instant = Some(std::time::Instant::now());
								play_voice_btn_space.set_label("Pause voice message");
							} else {
								if let Some(inst) = s.audio_start_instant.take() {
									s.audio_elapsed_secs += inst.elapsed().as_secs_f32() * s.audio_speed;
								}
								if let Some(ref sink) = s.audio_sink {
									sink.pause();
								}
								s.is_audio_paused = true;
								play_voice_btn_space.set_label("Play voice message");
							}
						} else {
							if let Some(ref sink) = s.audio_sink {
								sink.stop();
							}
							s.audio_sink = None;
							s.audio_elapsed_secs = 0.0;
							s.audio_start_instant = None;
							s.is_audio_paused = false;
							s.playing_msg_id = Some(msg.id.clone());
							play_voice_btn_space.set_label("Loading voice message...");
							let _ = tx_play_media.send(UiCommand::PlayVoiceMessage(msg.id.clone()));
						}
					}
				}
				event.skip(false);
			} else if pressed(message_list::VOTE) {
				if let Some(idx) = msgs_list_play.get_selection() {
					let s = state_play.borrow();
					if let Some(whatsnap::DisplayItem::Single(m)) = s.display_items.get(idx as usize)
						&& (m.is_poll() || m.is_event())
					{
						let _ = tx_close_list.send(UiCommand::OpenPollVote(m.id.clone()));
					}
				}
				event.skip(false);
			} else if pressed(message_list::REPLY) {
				if let Some(idx) = msgs_list_play.get_selection() {
					let mut s = state_play.borrow_mut();
					let msg_opt = s.display_items.get(idx as usize).and_then(|item| match item {
						whatsnap::DisplayItem::Single(m) => Some(m.clone()),
						whatsnap::DisplayItem::Album { messages, .. } => messages.first().cloned(),
					});
					if let Some(msg) = msg_opt
						&& !msg.is_date_divider
						&& !msg.is_deleted
					{
						let sender = if msg.from_me {
							"You".to_string()
						} else if !msg.sender_name.is_empty() {
							msg.sender_name.clone()
						} else {
							msg.sender_jid.clone()
						};
						let preview = if msg.is_audio || msg.text.starts_with("[Voice Message]") {
							"Voice message".to_string()
						} else if msg.text.is_empty() {
							"Media".to_string()
						} else {
							crate::ui::chat_ui::reply_snippet(&msg.text)
						};

						s.pending_reply_id = Some(msg.id.clone());
						s.pending_reply_sender =
							Some(if msg.from_me { "me".to_string() } else { msg.sender_jid.clone() });
						s.pending_reply_preview = Some(format!("Replying to {}: {}", sender, preview));

						let status_msg =
							format!("Replying to {}: {}. Type your reply and press Enter.", sender, preview);
						crate::platform::announce::announce(msgs_announcer, &status_msg);
						crate::ui::chat_ui::update_composer_buttons(&s, &conv_parts_play);
						msg_input_field.set_focus();
					}
				}
				event.skip(false);
			} else if pressed(message_list::EDIT) {
				if let Some(idx) = msgs_list_play.get_selection() {
					let s = state_play.borrow();
					let msg_opt = s.display_items.get(idx as usize).and_then(|item| match item {
						whatsnap::DisplayItem::Single(m) => Some(m.clone()),
						whatsnap::DisplayItem::Album { messages, .. } => messages.first().cloned(),
					});
					drop(s);
					if let Some(msg) = msg_opt
						&& !msg.is_date_divider
					{
						let now = std::time::SystemTime::now()
							.duration_since(std::time::UNIX_EPOCH)
							.unwrap_or_default()
							.as_secs() as i64;
						let in_channel =
							state_play.borrow().selected_chat.as_ref().is_some_and(|j| j.ends_with("@newsletter"));
						if !msg.is_editable(now, in_channel) {
							unsafe {
								let _ = windows::Win32::System::Diagnostics::Debug::MessageBeep(
									windows::Win32::UI::WindowsAndMessaging::MB_ICONHAND,
								);
							}
							let still_eligible_otherwise =
								msg.from_me && !msg.is_deleted && !msg.is_voice() && msg.edit_text().is_some();
							let reason = if still_eligible_otherwise {
								if in_channel {
									"This message can no longer be edited - the 30 day edit window has passed."
								} else {
									"This message can no longer be edited - the 15 minute edit window has passed."
								}
							} else {
								"Message can't be edited."
							};
							crate::platform::announce::announce(msgs_announcer, reason);
						} else {
							let _ = tx_play_media.send(UiCommand::BeginEditMessage {
								message_id: msg.id.clone(),
								text: msg.edit_text().unwrap_or_default().to_string(),
							});
						}
					}
				}
				event.skip(false);
			} else if pressed(message_list::STAR) {
				if let Some(idx) = msgs_list_play.get_selection() {
					let s = state_play.borrow();
					let jid = s.selected_chat.clone().unwrap_or_default();
					let item_opt = s.display_items.get(idx as usize).cloned();
					drop(s);
					if let Some(item) = item_opt {
						match item {
							whatsnap::DisplayItem::Single(msg) => {
								if !msg.is_date_divider {
									if crate::ui_commands::selection::can_star(&jid, &msg) {
										let sender_jid =
											if msg.from_me { "me".to_string() } else { msg.sender_jid.clone() };
										let _ = tx_play_media.send(UiCommand::StarMessage {
											message_id: msg.id.clone(),
											sender_jid,
											from_me: msg.from_me,
											starred: !msg.starred,
										});
									} else {
										crate::platform::announce::announce(msgs_announcer, "Message can't be starred.");
									}
								}
							}
							whatsnap::DisplayItem::Album { messages, .. } => {
								let can_any = messages.iter().any(|m| crate::ui_commands::selection::can_star(&jid, m));
								if can_any {
									let all_starred = !messages.is_empty() && messages.iter().all(|m| m.starred);
									let target_starred = !all_starred;
									for m in messages.iter().filter(|m| {
										m.starred != target_starred && crate::ui_commands::selection::can_star(&jid, m)
									}) {
										let sender_jid =
											if m.from_me { "me".to_string() } else { m.sender_jid.clone() };
										let _ = tx_play_media.send(UiCommand::StarMessage {
											message_id: m.id.clone(),
											sender_jid,
											from_me: m.from_me,
											starred: target_starred,
										});
									}
								} else {
									crate::platform::announce::announce(msgs_announcer, "Message can't be starred.");
								}
							}
						}
					}
				}
				event.skip(false);
			} else if pressed(message_list::FORWARD) {
				if let Some(idx) = msgs_list_play.get_selection() {
					let s = state_play.borrow();
					let msg_opt = s.display_items.get(idx as usize).and_then(|item| match item {
						whatsnap::DisplayItem::Single(m) => Some(m.clone()),
						whatsnap::DisplayItem::Album { messages, .. } => messages.first().cloned(),
					});
					if let Some(msg) = msg_opt
						&& !msg.is_date_divider
					{
						let mut labels = Vec::new();
						let mut jids = Vec::new();
						for jid in &s.chats {
							if !jid.ends_with("@broadcast") && crate::ui_commands::links::can_forward_to(&s, jid) {
								let name = s.contacts.get(jid).cloned().unwrap_or_else(|| jid.clone());
								labels.push(name);
								jids.push(jid.clone());
							}
						}
						for (jid, name) in &s.contacts {
							if !jid.ends_with("@broadcast")
								&& !jids.contains(jid)
								&& crate::ui_commands::links::can_forward_to(&s, jid)
							{
								labels.push(name.clone());
								jids.push(jid.clone());
							}
						}
						drop(s);

						if !jids.is_empty() {
							let dlg = Dialog::builder(&parent_frame, "Forward Message")
								.with_size(400, 450)
								.with_style(DialogStyle::DefaultDialogStyle | DialogStyle::ResizeBorder)
								.build();
							let dlg_panel = Panel::builder(&dlg).build();
							let dlg_sizer = BoxSizer::builder(Orientation::Vertical).build();

							let search_label =
								StaticText::builder(&dlg_panel).with_label("Search contacts or chats:").build();
							dlg_sizer.add(&search_label, 0, SizerFlag::Left | SizerFlag::Top | SizerFlag::Right, 8);

							let search_ctrl = TextCtrl::builder(&dlg_panel).build();
							dlg_sizer.add(&search_ctrl, 0, SizerFlag::Expand | SizerFlag::All, 8);

							let contacts_listbox = ListBox::builder(&dlg_panel).with_choices(labels.clone()).build();
							dlg_sizer.add(
								&contacts_listbox,
								1,
								SizerFlag::Expand | SizerFlag::Left | SizerFlag::Right,
								8,
							);

							let btn_panel = Panel::builder(&dlg_panel).build();
							let btn_sizer = BoxSizer::builder(Orientation::Horizontal).build();
							let ok_btn =
								Button::builder(&btn_panel).with_id(wxdragon::id::ID_OK).with_label("Forward").build();
							let cancel_btn = Button::builder(&btn_panel)
								.with_id(wxdragon::id::ID_CANCEL)
								.with_label("Cancel")
								.build();
							btn_sizer.add(&ok_btn, 0, SizerFlag::Right, 5);
							btn_sizer.add(&cancel_btn, 0, SizerFlag::empty(), 0);
							btn_panel.set_sizer(btn_sizer, true);

							dlg_sizer.add(&btn_panel, 0, SizerFlag::AlignRight | SizerFlag::All, 8);
							dlg_panel.set_sizer(dlg_sizer, true);

							let dlg_esc_search = dlg;
							search_ctrl.on_key_down(keys::escape_cancels(dlg_esc_search));

							let dlg_esc_list = dlg;
							contacts_listbox.on_key_down(keys::escape_cancels(dlg_esc_list));

							let current_labels = std::rc::Rc::new(std::cell::RefCell::new(labels.clone()));
							let current_jids = std::rc::Rc::new(std::cell::RefCell::new(jids.clone()));

							let all_labels = labels;
							let all_jids = jids;
							let current_labels_search = current_labels.clone();
							let current_jids_search = current_jids.clone();
							let listbox_search = contacts_listbox;

							search_ctrl.on_text_changed(move |_| {
								let q = search_ctrl.get_value().to_lowercase().trim().to_string();
								let mut fl = Vec::new();
								let mut fj = Vec::new();
								for (lbl, jid) in all_labels.iter().zip(all_jids.iter()) {
									if q.is_empty() || lbl.to_lowercase().contains(&q) {
										fl.push(lbl.clone());
										fj.push(jid.clone());
									}
								}
								listbox_search.clear();
								for l in &fl {
									listbox_search.append(l);
								}
								*current_labels_search.borrow_mut() = fl;
								*current_jids_search.borrow_mut() = fj;
							});

							let dlg_cancel = dlg;
							cancel_btn.on_click(move |_| {
								dlg_cancel.end_modal(0);
							});

							let dlg_ok = dlg;
							let tx_fwd = tx_close_list.clone();
							let msg_id = msg.id.clone();
							let current_jids_ok = current_jids;
							ok_btn.on_click(move |_| {
								if let Some(sel_idx) = contacts_listbox.get_selection() {
									let cj = current_jids_ok.borrow();
									if (sel_idx as usize) < cj.len() {
										let target_jid = cj[sel_idx as usize].clone();
										dlg_ok.end_modal(wxdragon::id::ID_OK);
										let _ = tx_fwd.send(UiCommand::ForwardMessage {
											message_id: msg_id.clone(),
											to_jids: vec![target_jid],
										});
									}
								}
							});

							search_ctrl.set_focus();
							dlg.show_modal();
							// The base Dialog has no Drop, so a dialog built per use must be
							// destroyed or it (and every closure bound to it) lives on hidden.
							dlg.destroy();
						}
					}
				}
				event.skip(false);
			} else if pressed(message_list::DELETE) {
				if let Some(idx) = msgs_list_play.get_selection() {
					let ids = deletable_ids(&state_play.borrow(), idx);
					if !ids.is_empty() {
						let _ = tx_play_media.send(UiCommand::AskDeleteMessages(ids));
					}
				}
			} else if pressed(message_list::VIEW_TEXT) && code != Some(WXK_RETURN) {
				show_message_text(&parent_frame, &msgs_list_play, &state_play);
				event.skip(false);
			} else if keys::is_context_menu_key(key_event) {
				open_menu_key.as_ref()();
				event.skip(false);
			} else {
				event.skip(true);
			}
		} else {
			event.skip(true);
		}
	});

	let tx_jump_enter = ui_tx.clone();
	let tx_phone_enter = ui_tx.clone();
	let tx_invite_enter = ui_tx.clone();
	let msgs_list_enter = parts.conversations_parts.messages_list;
	let state_enter = state.clone();
	let search_panel_enter = parts.conversations_parts.search_panel;
	let msgs_announcer_enter = parts.conversations_parts.messages_announcer;
	let parent_frame_enter = parts.frame;
	let enter_guard_msgs = crate::platform::tray::enter_release_guard();
	parts.conversations_parts.messages_list.on_key_up(move |event| {
		if let WindowEventData::Keyboard(ref key_event) = event {
			let code = key_event.get_key_code();
			// The release of an Enter pressed somewhere else (a menu, a
			// dialog, the Start menu) mustn't open the focused message's link.
			if keys::is_enter(code) && enter_guard_msgs.replace(false) {
				event.skip(false);
				return;
			}
			if code == Some(WXK_RETURN) && state_enter.borrow().keymap.is_pressed(message_list::VIEW_TEXT, key_event) {
				show_message_text(&parent_frame_enter, &msgs_list_enter, &state_enter);
				event.skip(false);
				return;
			}
			if keys::is_enter(code) {
				if search_panel_enter.is_shown() {
					if let Some(idx) = msgs_list_enter.get_selection() {
						let s = state_enter.borrow();
						let msg_opt = s.display_items.get(idx as usize).and_then(|item| match item {
							whatsnap::DisplayItem::Single(m) => Some(m.clone()),
							whatsnap::DisplayItem::Album { messages, .. } => messages.first().cloned(),
						});
						if let Some(msg) = msg_opt {
							drop(s);
							if !msg.is_date_divider {
								let _ = tx_jump_enter.send(UiCommand::JumpToSearchResult {
									message_id: msg.id.clone(),
									message_text: String::new(),
								});
							} else {
								let _ = tx_jump_enter.send(UiCommand::CloseSearch);
							}
						} else {
							drop(s);
							let _ = tx_jump_enter.send(UiCommand::CloseSearch);
						}
					} else {
						let _ = tx_jump_enter.send(UiCommand::CloseSearch);
					}
					event.skip(false);
				} else {
					if let Some(idx) = msgs_list_enter.get_selection() {
						let s = state_enter.borrow();
						let msg_opt = s.display_items.get(idx as usize).and_then(|item| match item {
							whatsnap::DisplayItem::Single(m) => Some(m.clone()),
							whatsnap::DisplayItem::Album { messages, .. } => messages.first().cloned(),
						});
						drop(s);
						if let Some(msg) = msg_opt {
							if msg.text.starts_with("[Group Invite]") {
								let _ = tx_invite_enter.send(UiCommand::OpenGroupInvite(msg.id.clone()));
								event.skip(false);
								return;
							}
							if msg.text.starts_with("[Channel Admin Invite]") {
								let _ = tx_invite_enter.send(UiCommand::OpenChannelAdminInvite(msg.id.clone()));
								event.skip(false);
								return;
							}
							if (msg.is_poll() || msg.is_event()) && !msg.is_deleted {
								let _ = tx_invite_enter.send(UiCommand::OpenPollVote(msg.id.clone()));
								event.skip(false);
								return;
							}
							if msg.is_order() {
								let _ = tx_invite_enter.send(UiCommand::OpenOrderDetails(msg.id.clone()));
								event.skip(false);
								return;
							}
							if msg.is_product() {
								let _ = tx_invite_enter.send(UiCommand::OpenCatalog(Some(msg.id.clone())));
								event.skip(false);
								return;
							}
							if msg.has_business_options() {
								let _ = tx_invite_enter.send(UiCommand::OpenMessageOptions(msg.id.clone()));
								event.skip(false);
								return;
							}
							let mut urls = Vec::new();
							let text = &msg.text;
							let mut start_idx = 0;
							while let Some(idx) = text[start_idx..].find("http") {
								let actual_idx = start_idx + idx;
								if text[actual_idx..].starts_with("http://")
									|| text[actual_idx..].starts_with("https://")
								{
									let space_idx = text[actual_idx..]
										.find(|c: char| c.is_whitespace())
										.unwrap_or(text.len() - actual_idx);
									let mut clean_url = &text[actual_idx..actual_idx + space_idx];
									while clean_url.ends_with(|c: char| c.is_ascii_punctuation()) {
										clean_url = &clean_url[..clean_url.len() - 1];
									}
									urls.push(clean_url.to_string());
									start_idx = actual_idx + space_idx;
								} else {
									start_idx = actual_idx + 4;
								}
							}

							let open_url = |url: &str| {
								if whatsnap::parse_whatsapp_link(url).is_some() {
									let _ = tx_invite_enter.send(UiCommand::OpenWhatsAppLink(url.to_string()));
								} else {
									wxdragon::utils::launch_default_browser(
										url,
										wxdragon::utils::BrowserLaunchFlags::Default,
									);
								}
							};
							if urls.len() == 1 {
								open_url(&urls[0]);
							} else if urls.len() > 1 {
								let url_strs: Vec<&str> = urls.iter().map(|s| s.as_str()).collect();
								let dialog = wxdragon::prelude::SingleChoiceDialog::builder(
									&msgs_list_enter,
									"Select a link to open:",
									"Open Link",
									&url_strs,
								)
								.build();
								if dialog.show_modal() == wxdragon::id::ID_OK
									&& let Some(selection) = dialog.get_string_selection()
								{
									open_url(&selection);
								}
							} else if let Some((_, digits)) = msg.first_phone_number() {
								let copy_num_id = 9001;
								let msg_num_id = 9002;
								let mut phone_menu = Menu::builder()
									.append_item(copy_num_id, "&Copy Number", "Copy phone number to clipboard")
									.append_item(msg_num_id, "&Message Number", "Start a chat with this phone number")
									.build();
								let tx_phone = tx_phone_enter.clone();
								let msgs_announcer_phone = msgs_announcer_enter;
								phone_menu.on_selected(move |menu_evt| {
									let sel_id = menu_evt.get_id();
									if sel_id == copy_num_id {
										if crate::copy_text(&format!("+{}", digits)) {
											crate::platform::announce::announce(
												msgs_announcer_phone,
												"Copied phone number to clipboard.",
											);
										}
									} else if sel_id == msg_num_id {
										let _ = tx_phone.send(UiCommand::OpenChatByPhone(digits.clone()));
									}
								});
								msgs_list_enter.popup_menu(&mut phone_menu, None);
								phone_menu.destroy_menu();
							}
						}
					}
					event.skip(false);
				}
			}
		}
	});

	let open_menu_cb = open_msg_context_menu.clone();
	parts.conversations_parts.messages_list.bind_internal(EventType::CONTEXT_MENU, move |_event| {
		open_menu_cb.as_ref()();
	});

	let tx_play_btn = ui_tx.clone();
	let msgs_list_btn = parts.conversations_parts.messages_list;
	let state_play_btn = state.clone();
	let play_voice_btn_ref = parts.conversations_parts.play_voice_btn;
	parts.conversations_parts.play_voice_btn.on_click(move |_| {
		if let Some(idx) = msgs_list_btn.get_selection() {
			let mut s = state_play_btn.borrow_mut();
			let msg_opt = s.display_items.get(idx as usize).and_then(|item| match item {
				whatsnap::DisplayItem::Single(m) => Some(m.clone()),
				whatsnap::DisplayItem::Album { messages, .. } => messages.first().cloned(),
			});
			if let Some(msg) = msg_opt {
				let is_currently_playing = s.playing_msg_id.as_deref() == Some(&msg.id);
				if is_currently_playing {
					if s.is_audio_paused {
						if let Some(ref sink) = s.audio_sink {
							sink.play();
						}
						s.is_audio_paused = false;
						s.audio_start_instant = Some(std::time::Instant::now());
						play_voice_btn_ref.set_label("Pause voice message");
						return;
					} else {
						// Pause — save elapsed time (played back at the
						// current speed since the last seek/start, so the
						// wall-clock delta must be scaled the same way the
						// resume/tick/seek paths compute position)
						if let Some(inst) = s.audio_start_instant.take() {
							s.audio_elapsed_secs += inst.elapsed().as_secs_f32() * s.audio_speed;
						}
						if let Some(ref sink) = s.audio_sink {
							sink.pause();
						}
						s.is_audio_paused = true;
						play_voice_btn_ref.set_label("Play voice message");
						return;
					}
				}

				if let Some(ref sink) = s.audio_sink {
					sink.stop();
				}
				s.audio_sink = None;
				s.audio_elapsed_secs = 0.0;
				s.audio_start_instant = None;
				s.is_audio_paused = false;
				s.playing_msg_id = Some(msg.id.clone());
				play_voice_btn_ref.set_label("Loading voice message...");
				let _ = tx_play_btn.send(UiCommand::PlayVoiceMessage(msg.id.clone()));
			}
		}
	});

	let state_sel = state.clone();
	let play_voice_btn_sel = parts.conversations_parts.play_voice_btn;
	let audio_slider_sel = parts.conversations_parts.audio_slider;
	let audio_speed_btn_sel = parts.conversations_parts.audio_speed_btn;
	let audio_progress_label_sel = parts.conversations_parts.audio_progress_label;
	let conv_parts_sel = parts.conversations_parts;
	let last_layout_time = std::cell::Cell::new(std::time::Instant::now());
	let tx_sel = ui_tx.clone();
	// The 100ms throttle below skips layout() on some events in a rapid
	// selection burst (e.g. holding an arrow key down through messages) so
	// the toolbar doesn't visibly freeze while it repaints on every row. But
	// if the burst's last event lands inside the throttle window, that final
	// show()/hide() never gets laid out - the toolbar is stuck showing the
	// previous row's buttons until some unrelated event happens to trigger
	// another layout. This one-shot timer, (re)armed on every skipped
	// layout, guarantees a trailing layout once the burst actually settles.
	// Typing in the message list starts a message when that's turned on.
	let state_typed = state.clone();
	let field_typed = parts.conversations_parts.message_field;
	parts.conversations_parts.messages_list.on_typed(move |c| {
		if !state_typed.borrow().settings.type_to_compose || !field_typed.is_shown() || !field_typed.is_enabled() {
			return false;
		}
		field_typed.set_focus();
		field_typed.set_insertion_point_end();
		field_typed.write_text(&c.to_string());
		true
	});

	let action_layout_timer = Timer::new(&parts.frame);
	let action_panel_for_timer = parts.conversations_parts.action_panel;
	action_layout_timer.on_tick(move |_| {
		action_panel_for_timer.layout();
	});
	parts.conversations_parts.messages_list.on_selection_changed(move |idx| {
		if let Ok(idx) = i32::try_from(idx) {
			const PREFETCH_ROWS: i32 = 50;
			let (older_stored, newer_stored) = {
				let s = state_sel.borrow();
				(s.older_unloaded > 0, !s.messages_at_latest)
			};
			let count = conv_parts_sel.messages_list.get_count() as i32;
			if idx == 0 || (older_stored && idx < PREFETCH_ROWS) {
				let _ = tx_sel.send(UiCommand::LoadMoreMessages);
			} else if newer_stored && idx + PREFETCH_ROWS >= count {
				let _ = tx_sel.send(UiCommand::LoadNewerMessages);
			}
			let s = state_sel.borrow();
			conv_parts_sel.action_panel.freeze();
			let mut need_layout = crate::ui::message_display::update_action_buttons(&s, &conv_parts_sel);

			let msg_opt = s.display_items.get(idx as usize).and_then(|item| match item {
				whatsnap::DisplayItem::Single(m) => Some(m.clone()),
				whatsnap::DisplayItem::Album { messages, .. } => messages.first().cloned(),
			});
			if let Some(msg) = msg_opt {
				let is_voice = match s.display_items.get(idx as usize) {
					Some(whatsnap::DisplayItem::Single(m)) => {
						let is_deleted = m.is_deleted
							|| m.text == "[This message was deleted]"
							|| m.text == "[You deleted this message]";
						!is_deleted
							&& (m.is_audio || m.text.starts_with("[Voice Message]") || m.text.starts_with("[Audio]"))
					}
					_ => false,
				};
				if play_voice_btn_sel.is_shown() != is_voice {
					play_voice_btn_sel.show(is_voice);
					need_layout = true;
				}
				if is_voice {
					let is_playing = s.playing_msg_id.as_deref() == Some(&msg.id);
					if is_playing {
						if let Some(ref sink) = s.audio_sink {
							if !sink.is_paused() && !sink.empty() {
								play_voice_btn_sel.set_label("Pause voice message");
							} else {
								play_voice_btn_sel.set_label("Play voice message");
							}
						} else {
							play_voice_btn_sel.set_label("Play voice message");
						}
						if !audio_slider_sel.is_shown() {
							audio_slider_sel.show(true);
							audio_speed_btn_sel.show(true);
							audio_speed_btn_sel.set_label(&format!("{:.1}x", s.audio_speed));
							audio_progress_label_sel.show(true);
							need_layout = true;
						}
					} else {
						play_voice_btn_sel.set_label("Play voice message");
						if audio_slider_sel.is_shown() {
							audio_slider_sel.show(false);
							audio_speed_btn_sel.show(false);
							audio_progress_label_sel.show(false);
							need_layout = true;
						}
					}
				} else if audio_slider_sel.is_shown() {
					audio_slider_sel.show(false);
					audio_speed_btn_sel.show(false);
					audio_progress_label_sel.show(false);
					need_layout = true;
				}
			} else {
				if play_voice_btn_sel.is_shown() {
					play_voice_btn_sel.show(false);
					need_layout = true;
				}
				if audio_slider_sel.is_shown() {
					audio_slider_sel.show(false);
					audio_speed_btn_sel.show(false);
					audio_progress_label_sel.show(false);
					need_layout = true;
				}
			}

			let any_action_visible = play_voice_btn_sel.is_shown()
				|| conv_parts_sel.action_retry_btn.is_shown()
				|| conv_parts_sel.action_cancel_upload_btn.is_shown()
				|| conv_parts_sel.action_open_btn.is_shown()
				|| conv_parts_sel.action_save_as_btn.is_shown()
				|| conv_parts_sel.action_download_btn.is_shown()
				|| audio_slider_sel.is_shown()
				|| audio_speed_btn_sel.is_shown();
			if conv_parts_sel.action_panel.is_shown() != any_action_visible {
				conv_parts_sel.action_panel.show(any_action_visible);
				need_layout = true;
			}

			conv_parts_sel.action_panel.thaw();

			if need_layout {
				let now = std::time::Instant::now();
				if now.duration_since(last_layout_time.get()) > std::time::Duration::from_millis(100) {
					conv_parts_sel.action_panel.layout();
					last_layout_time.set(now);
				} else {
					action_layout_timer.start(120, true);
				}
			}
		} else {
			let s = state_sel.borrow();
			conv_parts_sel.action_panel.freeze();
			let mut need_layout = crate::ui::message_display::update_action_buttons(&s, &conv_parts_sel);
			if play_voice_btn_sel.is_shown() {
				play_voice_btn_sel.show(false);
				need_layout = true;
			}
			if audio_slider_sel.is_shown() {
				audio_slider_sel.show(false);
				audio_speed_btn_sel.show(false);
				audio_progress_label_sel.show(false);
				need_layout = true;
			}

			if conv_parts_sel.action_panel.is_shown() {
				conv_parts_sel.action_panel.show(false);
				need_layout = true;
			}

			conv_parts_sel.action_panel.thaw();

			if need_layout {
				let now = std::time::Instant::now();
				if now.duration_since(last_layout_time.get()) > std::time::Duration::from_millis(100) {
					conv_parts_sel.action_panel.layout();
					last_layout_time.set(now);
				} else {
					action_layout_timer.start(120, true);
				}
			}
		}
	});

	let tx_toggle_search = ui_tx.clone();
	parts.conversations_parts.search_in_conv_btn.on_click(move |_| {
		let _ = tx_toggle_search.send(UiCommand::ToggleSearchPanel);
	});

	let tx_cancel_selection = ui_tx.clone();
	parts.conversations_parts.selection_cancel_btn.on_click(move |_| {
		let _ = tx_cancel_selection.send(UiCommand::ToggleSelectionMode);
	});

	let selection_list_toggle = parts.conversations_parts.messages_select_list;
	let conv_parts_selection_toggle = parts.conversations_parts;
	let selection_announcer = parts.conversations_parts.messages_announcer;
	let state_selection_toggle = state.clone();
	selection_list_toggle.on_toggled(move |idx| {
		let mut s = state_selection_toggle.borrow_mut();
		if let Some(id) = s.selection_rows.get(idx as usize).map(|item| item.id().to_string()) {
			if selection_list_toggle.is_checked(idx) {
				s.selected_message_ids.insert(id);
			} else {
				s.selected_message_ids.remove(&id);
			}
			crate::ui_commands::selection::update_bar(&s, &conv_parts_selection_toggle);
			let count = crate::ui_commands::selection::selected_messages(&s).len();
			crate::platform::announce::announce(selection_announcer, &selection_text(count));
		}
	});

	let tx_select_paging = ui_tx.clone();
	let state_select_paging = state.clone();
	parts.conversations_parts.messages_select_list.on_selection_changed(move |idx| {
		let (older_stored, newer_stored, count) = {
			let s = state_select_paging.borrow();
			(s.older_unloaded > 0, !s.messages_at_latest, s.selection_rows.len() as u32)
		};
		if idx == 0 && older_stored {
			let _ = tx_select_paging.send(UiCommand::LoadMoreMessages);
		} else if newer_stored && idx + 1 == count {
			let _ = tx_select_paging.send(UiCommand::LoadNewerMessages);
		}
	});

	// `messages_select_list` (not `messages_list`) has focus while selection
	// mode is active, so the selection-mode/Escape handling on `messages_list`
	// above never fires here - bind the same exit shortcuts on it directly.
	let tx_selection_list_keys = ui_tx.clone();
	let state_selection_list_keys = state.clone();
	parts.conversations_parts.messages_select_list.on_key_down(move |event| {
		if let WindowEventData::Keyboard(ref key_event) = event {
			let keymap = state_selection_list_keys.borrow().keymap.clone();
			if keymap.is_pressed(message_list::SELECTION_MODE, key_event) || keys::is_escape(key_event.get_key_code()) {
				let _ = tx_selection_list_keys.send(UiCommand::ToggleSelectionMode);
				event.skip(false);
				return;
			}
		}
		event.skip(true);
	});

	for (button, action) in [
		(parts.conversations_parts.selection_copy_btn, crate::ui_commands::selection::BarAction::Copy),
		(parts.conversations_parts.selection_star_btn, crate::ui_commands::selection::BarAction::Star),
		(parts.conversations_parts.selection_keep_btn, crate::ui_commands::selection::BarAction::Keep),
		(parts.conversations_parts.selection_delete_btn, crate::ui_commands::selection::BarAction::Delete),
		(parts.conversations_parts.selection_forward_btn, crate::ui_commands::selection::BarAction::Forward),
		(parts.conversations_parts.selection_save_btn, crate::ui_commands::selection::BarAction::Save),
	] {
		let tx_selection_action = ui_tx.clone();
		button.on_click(move |_| {
			let _ = tx_selection_action.send(UiCommand::SelectionAction(action));
		});
	}

	let tx_close_search = ui_tx.clone();
	parts.conversations_parts.search_close_btn.on_click(move |_| {
		let _ = tx_close_search.send(UiCommand::CloseSearch);
	});

	let tx_search_text = ui_tx.clone();
	let search_field_ref = parts.conversations_parts.search_conv_field;
	let msgs_list_search_focus = parts.conversations_parts.messages_list;
	search_field_ref.on_text_changed(move |_| {
		let query = search_field_ref.get_value();
		if !query.is_empty() {
			let _ = tx_search_text.send(UiCommand::SearchInConversation(query));
		}
	});
	let state_search_focus = state.clone();
	search_field_ref.on_text_enter(move |_| {
		msgs_list_search_focus.set_focus();
		let count = msgs_list_search_focus.get_count();
		if count > 0 {
			let s = state_search_focus.borrow();
			let mut selected_idx = 0;
			for (i, item) in s.display_items.iter().enumerate() {
				match item {
					whatsnap::DisplayItem::Single(m) => {
						if !m.is_date_divider {
							selected_idx = i as u32;
							break;
						}
					}
					whatsnap::DisplayItem::Album { .. } => {
						selected_idx = i as u32;
						break;
					}
				}
			}
			msgs_list_search_focus.set_selection(selected_idx, true);
		}
	});

	let tx_record = ui_tx.clone();
	parts.conversations_parts.record_voice_btn.on_click(move |_| {
		let _ = tx_record.send(UiCommand::StartRecording);
	});

	let tx_pause = ui_tx.clone();
	parts.conversations_parts.pause_resume_btn.on_click(move |_| {
		let _ = tx_pause.send(UiCommand::TogglePauseRecording);
	});

	let tx_preview = ui_tx.clone();
	parts.conversations_parts.preview_voice_btn.on_click(move |_| {
		let _ = tx_preview.send(UiCommand::ToggleVoicePreview);
	});

	let tx_send_voice = ui_tx.clone();
	parts.conversations_parts.send_voice_btn.on_click(move |_| {
		let _ = tx_send_voice.send(UiCommand::SendVoiceMessage);
	});

	let tx_discard_voice = ui_tx.clone();
	parts.conversations_parts.discard_voice_btn.on_click(move |_| {
		let _ = tx_discard_voice.send(UiCommand::DiscardVoiceMessage);
	});

	let tx_emoji = ui_tx.clone();
	parts.conversations_parts.emoji_btn.on_click(move |_| {
		let _ = tx_emoji.send(UiCommand::OpenEmojiPanel { stickers: false });
	});

	let tx_attach = ui_tx.clone();
	let attach_btn_menu = parts.conversations_parts.add_attachment_btn;
	parts.conversations_parts.add_attachment_btn.on_click(move |_| {
		const ATTACH_DOCUMENT_ID: i32 = 4201;
		const ATTACH_CONTACT_ID: i32 = 4202;
		const ATTACH_MEDIA_ID: i32 = 4203;
		const ATTACH_AUDIO_ID: i32 = 4204;
		const ATTACH_POLL_ID: i32 = 4205;
		const ATTACH_EVENT_ID: i32 = 4206;
		const ATTACH_STICKER_ID: i32 = 4207;
		let mut popup_menu = Menu::builder()
			.append_item(ATTACH_DOCUMENT_ID, "&Document", "Send any file as a document, photos at full quality")
			.append_item(ATTACH_MEDIA_ID, "&Photos and videos", "Send photos and videos")
			.append_item(ATTACH_AUDIO_ID, "&Audio", "Send audio files to play in the chat")
			.append_item(ATTACH_CONTACT_ID, "&Contact", "Attach a contact from your WhatsApp contacts")
			.append_item(ATTACH_POLL_ID, "P&oll", "Create a poll or a quiz")
			.append_item(ATTACH_EVENT_ID, "&Event", "Create an event people can answer")
			.append_item(
				ATTACH_STICKER_ID,
				"&Sticker",
				"Send a favorite, recent or WhatsApp sticker, or a sticker file",
			)
			.build();
		let tx_menu = tx_attach.clone();
		popup_menu.on_selected(move |menu_evt| {
			let id = menu_evt.get_id();
			let cmd = match id {
				ATTACH_DOCUMENT_ID => Some(UiCommand::AttachFiles(whatsnap::AttachKind::Document)),
				ATTACH_MEDIA_ID => Some(UiCommand::AttachFiles(whatsnap::AttachKind::Media)),
				ATTACH_AUDIO_ID => Some(UiCommand::AttachFiles(whatsnap::AttachKind::Audio)),
				ATTACH_CONTACT_ID => Some(UiCommand::PrepareAttachContact),
				ATTACH_POLL_ID => Some(UiCommand::OpenPollDialog),
				ATTACH_EVENT_ID => Some(UiCommand::OpenEventDialog),
				ATTACH_STICKER_ID => Some(UiCommand::OpenEmojiPanel { stickers: true }),
				_ => None,
			};
			if let Some(cmd) = cmd {
				let _ = tx_menu.send(cmd);
			}
		});
		attach_btn_menu.popup_menu(&mut popup_menu, None);
		popup_menu.destroy_menu();
	});

	let tx_speed = ui_tx.clone();
	parts.conversations_parts.audio_speed_btn.on_click(move |_| {
		let _ = tx_speed.send(UiCommand::ToggleAudioSpeed);
	});

	let tx_send_attach = ui_tx.clone();
	let caption_field_ref = parts.attachment_dialog.caption_field;
	parts.attachment_dialog.send_btn.on_click(move |_| {
		let caption = caption_field_ref.get_value();
		let _ = tx_send_attach.send(UiCommand::SendAttachment { caption });
	});
	let tx_send_attach_enter = ui_tx.clone();
	caption_field_ref.on_text_enter(move |_| {
		let caption = caption_field_ref.get_value();
		let _ = tx_send_attach_enter.send(UiCommand::SendAttachment { caption });
	});

	let tx_add_more = ui_tx.clone();
	parts.attachment_dialog.add_more_btn.on_click(move |_| {
		let _ = tx_add_more.send(UiCommand::AddAttachment);
	});

	let tx_discard_attach = ui_tx.clone();
	parts.attachment_dialog.discard_btn.on_click(move |_| {
		let _ = tx_discard_attach.send(UiCommand::DiscardAttachment);
	});

	let tx_retry = ui_tx.clone();
	parts.conversations_parts.action_retry_btn.on_click(move |_| {
		let _ = tx_retry.send(UiCommand::RetrySelectedMessage);
	});

	let tx_cancel_upload = ui_tx.clone();
	parts.conversations_parts.action_cancel_upload_btn.on_click(move |_| {
		let _ = tx_cancel_upload.send(UiCommand::CancelSelectedUpload);
	});

	let tx_download = ui_tx.clone();
	parts.conversations_parts.action_download_btn.on_click(move |_| {
		let _ = tx_download.send(UiCommand::DownloadMedia);
	});

	let tx_open_media = ui_tx.clone();
	parts.conversations_parts.action_open_btn.on_click(move |_| {
		let _ = tx_open_media.send(UiCommand::OpenMedia);
	});

	let tx_save_as = ui_tx.clone();
	parts.conversations_parts.action_save_as_btn.on_click(move |_| {
		let _ = tx_save_as.send(UiCommand::SaveMediaAs);
	});

	let tx_cancel_edit = ui_tx.clone();
	parts.conversations_parts.cancel_edit_btn.on_click(move |_| {
		let _ = tx_cancel_edit.send(UiCommand::CancelEdit);
	});

	let msg_field_shift_tab = parts.conversations_parts.message_field;
	let send_btn_tab = parts.conversations_parts.send_message_btn;
	parts.conversations_parts.cancel_edit_btn.on_key_down(move |event| {
		if let WindowEventData::Keyboard(ref key_event) = event
			&& keys::is_tab(key_event.get_key_code())
		{
			if key_event.shift_down() {
				msg_field_shift_tab.set_focus();
			} else {
				send_btn_tab.set_focus();
			}
			event.skip(false);
			return;
		}
		event.skip(true);
	});

	let tx_remove_quote = ui_tx.clone();
	parts.conversations_parts.remove_quote_btn.on_click(move |_| {
		let _ = tx_remove_quote.send(UiCommand::RemoveQuote);
	});

	let tx_read_more = ui_tx.clone();
	parts.conversations_parts.read_more_btn.on_click(move |_| {
		let _ = tx_read_more.send(UiCommand::LoadMoreMessages);
	});

	let tx_slider = ui_tx.clone();
	parts.conversations_parts.audio_slider.on_slider(move |event| {
		let val = event.get_value();
		let _ = tx_slider.send(UiCommand::SeekAudioAbsolute(val));
	});

	let tx_slider_key = ui_tx.clone();
	parts.conversations_parts.audio_slider.on_key_down(move |event| {
		if let WindowEventData::Keyboard(ref key) = event {
			let code = key.get_key_code();
			if keys::is_left(code) || keys::is_down(code) || keys::is_home(code) {
				let _ = tx_slider_key.send(UiCommand::SeekAudio(-5.0));
				event.skip(false);
			} else if keys::is_right(code) || keys::is_up(code) || keys::is_end(code) {
				let _ = tx_slider_key.send(UiCommand::SeekAudio(5.0));
				event.skip(false);
			} else {
				event.skip(true);
			}
		} else {
			event.skip(true);
		}
	});
}

fn choose_excluded_contacts(
	state: &std::rc::Rc<std::cell::RefCell<crate::AppState>>,
	dialog: Dialog,
	control: &crate::ui::dialogs::settings_dialog::PrivacyControl,
) {
	let tell = |message: &str| {
		MessageDialog::builder(&dialog, message, "Settings")
			.with_style(MessageDialogStyle::OK | MessageDialogStyle::IconInformation)
			.build()
			.show_modal();
	};
	let start: Result<(Vec<crate::Contact>, Vec<crate::Contact>), String> = {
		let s = state.borrow();
		let loaded = s.privacy_lists.get(control.name);
		let is_except = s.privacy_settings.get(control.name).map(String::as_str)
			== Some(crate::ui::dialogs::settings_dialog::EXCEPT_VALUE);
		let excluded = if let Some(jids) = s.privacy_list_edits.get(control.name) {
			let known = loaded.and_then(|l| l.as_ref().ok());
			Ok(jids
				.iter()
				.map(|jid| {
					s.all_contacts
						.iter()
						.chain(known.into_iter().flatten())
						.find(|c| c.jid == *jid)
						.cloned()
						.unwrap_or_else(|| crate::Contact {
							jid: jid.clone(),
							name: String::new(),
							phone: String::new(),
						})
				})
				.collect())
		} else {
			match loaded {
				Some(Ok(list)) => Ok(list.clone()),
				Some(Err(_)) if !is_except => Ok(Vec::new()),
				Some(Err(_)) => Err(format!(
					"Couldn't load who's excluded from {} right now, so it can't be changed. Try again later.",
					control.label
				)),
				None => Err(format!("Still loading who's excluded from {}. Try again in a moment.", control.label)),
			}
		};
		excluded.map(|excluded| (s.all_contacts.clone(), excluded))
	};
	let (contacts, excluded) = match start {
		Ok(start) => start,
		Err(message) => {
			tell(&message);
			return;
		}
	};
	if let Some(jids) =
		crate::ui::dialogs::privacy_list_picker::pick_excluded_contacts(&dialog, control.label, &contacts, &excluded)
	{
		state.borrow_mut().privacy_list_edits.insert(control.name.to_string(), jids);
	}
}

/// Account, Log Out (also the Log Out button in Settings): asks first, then
/// logs the account on screen out. True when it went ahead.
fn confirm_logout(parent: &impl WxWidget, state: &Rc<std::cell::RefCell<crate::AppState>>) -> bool {
	let name = {
		let accounts = state.borrow().accounts.clone();
		let a = accounts.borrow();
		(a.registry.live_count() > 1).then(|| a.registry.display_name(&a.registry.active))
	};
	let question = match name {
		Some(name) => format!(
			"Log {name} out of WhatsApp on this computer? You'll need to scan a QR code or enter its phone number to use it in whatSnap again."
		),
		None => "Log out of WhatsApp on this computer? You'll need to scan a QR code or re-enter your phone number to use whatSnap again.".to_string(),
	};
	let dlg = MessageDialog::builder(parent, &question, "Log Out")
		.with_style(MessageDialogStyle::OK | MessageDialogStyle::Cancel | MessageDialogStyle::IconQuestion)
		.build();
	if dlg.show_modal() != wxdragon::id::ID_OK {
		return false;
	}
	let _ = state.borrow().ws_tx.send(serde_json::json!({"action": "logout"}).to_string());
	true
}
