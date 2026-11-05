// SPDX-License-Identifier: MPL-2.0

//! Composed widgets: the pieces of interface that more than one view needs.
//!
//! **This crate names no node, no workspace and no document.** A widget takes
//! plain data in and reports a plain outcome out; anything that needs a `NodeId`
//! to work is not a widget and belongs to the view that owns it. The manifest
//! enforces it — the dependency list is `koru-draw`, `vello`, `winit`, the
//! two [`text_field`] brought with it (`parley` for the editor it drives,
//! `koru-clipboard` for plain text), and the two [`date_grid`] brought
//! with it (`koru-when` for the phrase box's grammar, `chrono` for the day it
//! answers). If it ever grows an `koru-core` the rule has already been broken.
//!
//! Two things fall out of that, and both are the point:
//!
//! - **every widget is constructible in a test without a document**, so almost
//!   everything in here is pinned by pure geometry — draw it, ask where a
//!   control went, ask what a click there hits, assert the two agree;
//! - **every widget can be rendered on its own**, which is what makes the
//!   component gallery in `docs/contributing/techdebt/README.md` possible at all. Being a crate
//!   rather than a module inside `apps/koru` is the whole of that difference.
//!
//! Where the line with `koru-draw` falls: that crate draws *shapes* and this
//! one draws *things*. `scene::floating_panel` will give you a panel with a
//! shadow; it does not know the panel is a confirmation dialog. The dividing
//! question is whether it has state or is a function of its arguments. See
//! `README.md` in both crates.

pub mod badge;
pub mod button;
pub mod button_group;
pub mod caption;
pub mod card;
pub mod chip;
pub mod choice;
pub mod clipboard;
pub mod color_picker_row;
pub mod confirm;
pub mod copy_affordance;
pub mod cursor;
pub mod date_grid;
pub mod dates;
pub mod dialog_frame;
pub mod dictation;
pub mod divider;
pub mod dropdown;
pub mod exit_list;
pub mod file_icon;
pub mod font_preview;
pub mod highlighted_text;
pub mod icon_button;
pub mod key_chip;
pub mod label_chip;
pub mod keys;
pub mod logo;
pub mod measurement_badge;
pub mod media_viewer;
pub mod menu;
pub mod modal;
pub mod nav_list;
pub mod page_shell;
pub mod place_chooser;
pub mod popover;
pub mod preview_tile;
pub mod profile_picker;
pub mod progress_bar;
pub mod prompt;
pub mod pulsing_dots;
pub mod scrollbar;
pub mod segmented;
pub mod selectable_list;
pub mod stepper;
pub mod swatch;
pub mod table;
pub mod text_field;
pub mod text_input;
pub mod text_paragraph;
pub mod text_title;
pub mod toast;
pub mod toggle;
pub mod tooltip;
mod typography;
pub mod welcome;

pub use badge::NumericBadge;
pub use button::{ActionButton, ActionStyle, ButtonRole, draw_button};
pub use card::{Card, CardRow};
pub use chip::Chip;
pub use choice::{Choice, ChoiceOutcome};
pub use color_picker_row::ColorPickerRow;
pub use confirm::Confirm;
pub use copy_affordance::{CopyAffordance, Outcome as CopyOutcome};
pub use date_grid::{DateGrid, Pick};
pub use dialog_frame::{KoruDialogFrame, KoruDialogFrameInspection, KoruDialogFrameState};
pub use dictation::DictationTarget;
pub use dropdown::{Dropdown, DropdownOption};
pub use exit_list::ExitList;
pub use font_preview::{FontPreview, FontSample};
pub use key_chip::KeyChip;
pub use label_chip::LabelChip;
pub use keys::{ctrl_letter, recover_modified_editing_key};
pub use measurement_badge::{KoruMeasurementBadge, KoruMeasurementGeometry};
pub use menu::{Anchor, Control, Menu, MenuCard, MenuItem};
pub use nav_list::{ChipNav, NavItem, NavLeading, NavList};
pub use page_shell::PageShell;
pub use place_chooser::{Place, PlaceChooser};
pub use popover::{PopoverAlign, PopoverShell};
pub use preview_tile::PreviewTile;
pub use progress_bar::{ProgressBar, ProgressMode};
pub use pulsing_dots::PulsingDots;
pub use segmented::Segmented;
pub use stepper::{Step, Stepper};
pub use swatch::Swatch;
pub use table::{Band, Column, Metrics, RowState, Table};
pub use text_field::{TextArea, TextField, Typed};
pub use text_input::{EndIcon, InputStyle, TextInput};
pub use text_title::KoruTextTitle;
pub use toast::Toast;
pub use toggle::Toggle;
pub use tooltip::{Tooltip, TooltipContent};
