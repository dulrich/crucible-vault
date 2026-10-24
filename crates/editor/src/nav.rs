//! Day/month nav bars (WP-4), replacing Note Toolbar's `{{js:}}` bars (inventory #2).
//!
//! Items come from `editor_model::period::bar_for` (transcribed from the live Note Toolbar
//! settings). Period targets run `period.open`, which materializes the note and returns
//! `open_path`; the result opens in the bar's own pane — the bar belongs to the pane, not the
//! "active file", which drops Note Toolbar's wrong-nav-in-split caveat.

use chrono::NaiveDate;
use cv_protocol::{ActionContext, RunActionParams};
use editor_model::period::{self, Period};
use iced::widget::pane_grid::Pane;
use iced::widget::{button, row, text};
use iced::{Element, Length};

use crate::message::{Message, NavMsg};
use crate::theme::Tokens;

/// The `period.open` call.
pub fn period_params(p: Period, date: NaiveDate) -> RunActionParams {
    RunActionParams {
        action_id: "period.open".into(),
        params: serde_json::Map::from_iter([
            (
                "period".to_owned(),
                serde_json::Value::String(p.as_str().to_owned()),
            ),
            (
                "date".to_owned(),
                serde_json::Value::String(period::iso(date)),
            ),
        ]),
        context: ActionContext::default(),
    }
}

/// The bar for `pane`'s active tab on `path`, if that path gets one.
pub fn bar<'a>(
    pane: Pane,
    path: &str,
    today: NaiveDate,
    folders: &period::PeriodFolders,
    links: &[period::NavLink],
    tokens: Tokens,
) -> Option<Element<'a, Message>> {
    let items = period::bar_for(path, today, folders, links)?;
    let buttons = items.into_iter().map(|item| {
        button(
            text(item.label)
                .size(14)
                .color(tokens.accent_text.to_color()),
        )
        .style(button::text)
        .on_press(Message::Nav(NavMsg::Go {
            pane,
            target: item.target,
        }))
        .into()
    });
    Some(row(buttons).spacing(8).width(Length::Fill).into())
}
