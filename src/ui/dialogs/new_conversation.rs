use wxdragon::prelude::*;

use crate::ui::{
	dialog_look::{self, Field},
	theme,
};

#[allow(dead_code)]
pub struct NewConversationDialogParts {
	pub dialog: Dialog,
	pub search_label: StaticText,
	pub search_field: TextCtrl,
	pub action_panel: Panel,
	pub new_group_btn: Button,
	pub new_community_btn: Button,
	pub new_contact_btn: Button,
	pub message_meta_ai_btn: Button,
	pub contacts_label: StaticText,
	pub results_list: ListBox,
	pub close_btn: Button,
	pub announcer: StaticText,
}

pub fn build_new_conversation_dialog(parent: &impl WxWidget) -> NewConversationDialogParts {
	let dialog = Dialog::builder(parent, "New Chat")
		.with_size(540, 620)
		.with_style(DialogStyle::DefaultDialogStyle | DialogStyle::ResizeBorder)
		.build();
	theme::style_content(&dialog);

	let panel = Panel::builder(&dialog).build();
	theme::style_content(&panel);
	let sizer = BoxSizer::builder(Orientation::Vertical).build();

	let search_label = StaticText::builder(&panel).with_label("Search name or number").build();
	theme::style_muted_label(&search_label);
	sizer.add(&search_label, 0, SizerFlag::Left | SizerFlag::Top, 14);

	let search_field =
		TextCtrl::builder(&panel).with_style(dialog_look::field_style(TextCtrlStyle::ProcessEnter)).build();
	dialog_look::add_field(&sizer, &search_field, Field::Search, 0);

	let action_panel = Panel::builder(&panel).build();
	theme::style_content(&action_panel);
	let action_sizer = BoxSizer::builder(Orientation::Vertical).build();
	let bg = theme::Palette::CONTENT_BG();

	let row1 = BoxSizer::builder(Orientation::Horizontal).build();
	let new_group_btn = Button::builder(&action_panel).with_label("New Group").build();
	dialog_look::pill_button(&new_group_btn, false, bg);
	row1.add(&new_group_btn, 0, SizerFlag::Right, 8);

	let new_community_btn = Button::builder(&action_panel).with_label("New Community").build();
	dialog_look::pill_button(&new_community_btn, false, bg);
	row1.add(&new_community_btn, 0, SizerFlag::empty(), 0);
	action_sizer.add_sizer(&row1, 0, SizerFlag::Expand | SizerFlag::Bottom, 6);

	let row2 = BoxSizer::builder(Orientation::Horizontal).build();
	let new_contact_btn = Button::builder(&action_panel).with_label("New Contact").build();
	dialog_look::pill_button(&new_contact_btn, false, bg);
	row2.add(&new_contact_btn, 0, SizerFlag::Right, 8);

	let message_meta_ai_btn = Button::builder(&action_panel).with_label("Message Meta AI").build();
	dialog_look::pill_button(&message_meta_ai_btn, false, bg);
	row2.add(&message_meta_ai_btn, 0, SizerFlag::empty(), 0);
	action_sizer.add_sizer(&row2, 0, SizerFlag::Expand, 0);

	action_panel.set_sizer(action_sizer, true);
	sizer.add(&action_panel, 0, SizerFlag::Expand | SizerFlag::Left | SizerFlag::Right | SizerFlag::Top, 10);

	let contacts_label = StaticText::builder(&panel).with_label("Contacts on WhatsApp").build();
	contacts_label.set_font(&theme::Fonts::body_strong());
	theme::style_muted_label(&contacts_label);
	sizer.add(&contacts_label, 0, SizerFlag::Left | SizerFlag::Top, 12);

	let results_list =
		ListBox::builder(&panel).with_style(ListBoxStyle::from_bits_retain(crate::ui::chat_rows::OWNER_DRAW)).build();
	results_list.set_font(&theme::Fonts::body());
	sizer.add(&results_list, 1, SizerFlag::Expand | SizerFlag::Left | SizerFlag::Right | SizerFlag::Top, 6);

	let announcer = StaticText::builder(&panel).with_label("").build();
	live_region::set_live_region(&announcer);
	theme::style_muted_label(&announcer);
	sizer.add(&announcer, 0, SizerFlag::Left | SizerFlag::Top, 4);

	let bottom_sizer = BoxSizer::builder(Orientation::Horizontal).build();
	bottom_sizer.add_stretch_spacer(1);
	let close_btn = Button::builder(&panel).with_id(ID_CANCEL).with_label("Close").build();
	dialog_look::pill_button(&close_btn, false, bg);
	bottom_sizer.add(&close_btn, 0, SizerFlag::empty(), 0);

	sizer.add_sizer(&bottom_sizer, 0, SizerFlag::Expand | SizerFlag::All, 12);

	panel.set_sizer(sizer, true);
	dialog_look::paint_fields(&panel, &[(search_field, Field::Search)]);

	let outer = BoxSizer::builder(Orientation::Vertical).build();
	outer.add(&panel, 1, SizerFlag::Expand, 0);
	dialog.set_sizer(outer, true);

	dialog.center();

	let dlg_esc1 = dialog;
	search_field.on_key_down(crate::ui::keys::escape_cancels(dlg_esc1));

	NewConversationDialogParts {
		dialog,
		search_label,
		search_field,
		action_panel,
		new_group_btn,
		new_community_btn,
		new_contact_btn,
		message_meta_ai_btn,
		contacts_label,
		results_list,
		close_btn,
		announcer,
	}
}
