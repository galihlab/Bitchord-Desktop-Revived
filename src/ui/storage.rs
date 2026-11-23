//! Settings › General › Storage, in Mochy's shape: how much room the app takes on
//! each disk and what of it is cache, songs and settings, with the buttons that cut
//! it down. The cards, bars, legends and pills are Mochy's own design.

use egui::{Color32, Sense, Ui, pos2, vec2};

use crate::cache::{self, Drive, Usage};
use crate::theme;

use super::{App, Dialog, Page, pages, widgets};

// Mochy's storage palette, with light counterparts for the app's second mode: the
// cards sit on the page's own background, so their fill is a card's rather than the
// settings glass.
fn card_fill() -> Color32 {
    if theme::light() { Color32::WHITE } else { Color32::from_rgb(0x1C, 0x1C, 0x1E) }
}

/// The bars' empty track.
fn track() -> Color32 {
    if theme::light() { Color32::from_rgb(0xE9, 0xE9, 0xEB) } else { Color32::from_rgb(0x3A, 0x3A, 0x3C) }
}

/// Numbers and hints that stand back from the labels.
fn muted() -> Color32 {
    if theme::light() { Color32::from_rgb(0x8A, 0x86, 0x94) } else { Color32::from_rgb(0x8E, 0x8E, 0x93) }
}

/// A button that throws things away.
fn danger_color() -> Color32 {
    if theme::light() { Color32::from_rgb(0xD6, 0x45, 0x3A) } else { Color32::from_rgb(0xF0, 0x7A, 0x70) }
}

/// BitChord's part of a bar, and the rest of what's used on the disk.
const GREEN: Color32 = Color32::from_rgb(0x86, 0xEF, 0x7F);
const OTHERS: Color32 = Color32::from_rgb(0x6E, 0x6E, 0x73);
/// The cache and the app's own files, in the breakdown.
const ORANGE: Color32 = Color32::from_rgb(0xF5, 0xB2, 0x6B);
const BLUE: Color32 = Color32::from_rgb(0x68, 0x78, 0xF0);

/// The cards' roundness, as Mochy draws them.
const CARD_RADIUS: u8 = 24;

/// Settings › Storage: how much room the app takes, and the ways to give it back.
pub(super) fn settings_section(app: &mut App, ui: &mut Ui) {
    // Said once the work lands; the toasts outlive the section, which a search hides.
    if let Some(freed) = app.storage.take_freed() {
        app.toast(format!("Cache cleared: {} freed", cache::format_size(freed)));
    }
    if let Some((songs, bytes)) = app.storage.take_removed() {
        app.toast(format!("Removed {songs} songs, {} freed", cache::format_size(bytes)));
    }
    if !pages::filter_show(ui, "Storage cache disk space covers lyrics downloads remove clear junk folder") {
        return;
    }
    let now = ui.input(|i| i.time);
    let folder = crate::downloads::folder();
    let usage = app.storage.usage(ui.ctx(), app.backend.data_dir(), &folder, now);
    let busy = app.storage.busy();
    let count = crate::downloads::count();
    let id = egui::Id::unique("storage");

    pages::section_label(ui, "Storage", None);

    // One card per disk the app keeps things on, the PC's own first.
    match usage.as_ref().map(|u| u.drives.as_slice()) {
        None => overall_card(ui, id, "Overall", None),
        Some(drives) => {
            for (i, drive) in drives.iter().enumerate() {
                if i > 0 {
                    ui.add_space(12.0);
                }
                overall_card(ui, id.with(i), if i == 0 { "Overall" } else { drive.label.as_str() }, Some(drive));
            }
        }
    }

    ui.add_space(22.0);
    let headline = match usage.as_ref() {
        Some(u) if u.drives.len() == 1 => {
            let percent = percent_of(u.total(), u.drives[0].total);
            format!("BitChord uses {} of your storage ({percent}), which is split into", cache::format_size(u.total()))
        }
        Some(u) => format!("BitChord uses {} across {} disks, which is split into", cache::format_size(u.total()), u.drives.len()),
        None => "Working out how much room BitChord takes…".to_owned(),
    };
    section_text(ui, &headline);
    ui.add_space(14.0);
    breakdown_card(ui, id.with("breakdown"), usage.as_ref());

    ui.add_space(26.0);
    section_text(ui, "So, here's a list of things you can do to cut the junk");
    if let Some(u) = usage.as_ref() {
        smart_hint(ui, u, count);
    }

    // The things to do about it, most drastic first (Mochy's ClearButtons).
    ui.add_space(14.0);
    let mut ask = None;
    if action_button(ui, "Remove all downloads", true, !busy && count > 0, None) {
        ask = Some(Dialog::ConfirmRemoveDownloads);
    }
    ui.add_space(10.0);
    if action_button(ui, "Remove all cover & lyrics cache", true, !busy, None) {
        ask = Some(Dialog::ConfirmClearCache);
    }
    ui.add_space(10.0);
    if action_button(ui, "Manage downloads", false, count > 0, None) {
        app.go(Page::Downloads);
    }
    if let Some(d) = ask {
        app.dialog = Some(d);
    }

    ui.add_space(26.0);
    section_text(ui, "Or");
    ui.add_space(14.0);
    // The folder names the one place besides the data folder the app keeps songs; a
    // pick is measured as soon as it lands.
    let detail = folder.display().to_string();
    if action_button(ui, "Choose where downloads are saved", false, !busy, Some(&detail)) {
        crate::downloads::pick_folder(ui.ctx());
    }
}

/// A disk's card: how much of it the app takes, against everything else on it
/// (Mochy's OverallCard; the PC's own disk is the "Overall" one).
fn overall_card(ui: &mut Ui, id: egui::Id, title: &str, drive: Option<&Drive>) {
    storage_card(ui, |ui| {
        ui.label(egui::RichText::new(title).font(theme::bold(17.0)).color(theme::text()));
        ui.add_space(14.0);
        let parts = [
            (GREEN, drive.map_or(0, |d| d.app)),
            (OTHERS, drive.map_or(0, |d| d.others())),
        ];
        usage_bar(ui, id.with("bar"), &parts, drive.map_or(0, |d| d.total));
        ui.add_space(14.0);
        // The line under: what's the app's, what's everything else's, what's spare.
        let (rect, _) = ui.allocate_exact_size(vec2(ui.available_width(), 22.0), Sense::hover());
        let p = ui.painter();
        let swatch = |p: &egui::Painter, x: f32, color: Color32| {
            p.rect_filled(egui::Rect::from_min_size(pos2(x, rect.center().y - 10.0), vec2(20.0, 20.0)), 6.0, color);
        };
        let mut x = rect.left();
        swatch(p, x, GREEN);
        let g = p.layout_no_wrap("BitChord".to_owned(), theme::semibold(16.0), theme::text());
        p.galley(pos2(x + 28.0, rect.center().y - g.size().y / 2.0), g.clone(), theme::text());
        x += 28.0 + g.size().x + 16.0;
        swatch(p, x, OTHERS);
        let g = p.layout_no_wrap("Your storage".to_owned(), theme::semibold(16.0), theme::text());
        p.galley(pos2(x + 28.0, rect.center().y - g.size().y / 2.0), g, theme::text());
        if let Some(d) = drive {
            let free = format!("{} free", cache::format_size(d.free));
            let g = p.layout_no_wrap(free, theme::regular(12.0), muted());
            p.galley(pos2(rect.right() - g.size().x, rect.center().y - g.size().y / 2.0), g, muted());
        }
    });
}

/// The breakdown card: the app's room split into its three parts (Mochy's
/// BreakdownCard, without the database it has and this app doesn't).
fn breakdown_card(ui: &mut Ui, id: egui::Id, usage: Option<&Usage>) {
    let (cache, songs, data) = match usage {
        Some(u) => (u.cache, u.downloads, u.app_data),
        None => (0, 0, 0),
    };
    storage_card(ui, |ui| {
        ui.label(egui::RichText::new("BitChord app data usage").font(theme::bold(17.0)).color(theme::text()));
        ui.add_space(14.0);
        usage_bar(ui, id.with("bar"), &[(ORANGE, cache), (GREEN, songs), (BLUE, data)], cache + songs + data);
        ui.add_space(16.0);
        // A measure still running leaves the sizes off, as Mochy's do.
        let size = |bytes: u64| usage.map(|_| cache::format_size(bytes));
        legend_row(ui, id.with("cache"), ORANGE, "Cache", size(cache).as_deref());
        ui.add_space(10.0);
        legend_row(ui, id.with("songs"), GREEN, "Downloaded music", size(songs).as_deref());
        ui.add_space(10.0);
        legend_row(ui, id.with("data"), BLUE, "App data", size(data).as_deref());
    });
}

/// One line pointing at the biggest easy win, and a nudge when the disk is nearly
/// full (Mochy's SmartHint).
fn smart_hint(ui: &mut Ui, usage: &Usage, download_count: usize) {
    let disk = &usage.drives[0];
    let nearly_full = disk.total > 0 && disk.free * 10 < disk.total;
    let hint = if usage.cache >= usage.downloads && usage.cache > 50_000_000 {
        Some(format!("The cache is the easy one. Clearing it frees {} and everything in it comes back on its own.", cache::format_size(usage.cache)))
    } else if download_count > 0 && usage.downloads > 50_000_000 {
        Some("Downloads take the most room; removing them all frees the lot in one go.".to_owned())
    } else {
        None
    };
    let mut text = String::new();
    if nearly_full {
        text.push_str("Your disk is nearly full.");
    }
    if let Some(h) = hint {
        if !text.is_empty() {
            text.push(' ');
        }
        text.push_str(&h);
    }
    if text.is_empty() {
        return;
    }
    let g = widgets::galley_wrapped(ui, &text, theme::regular(13.0), muted(), ui.available_width() - 4.0, 3);
    let (rect, _) = ui.allocate_exact_size(vec2(ui.available_width(), g.size().y + 6.0), Sense::hover());
    ui.painter().galley(pos2(rect.left() + 2.0, rect.top() + 6.0), g, muted());
}

/// A heading between the cards (Mochy's SectionText).
fn section_text(ui: &mut Ui, text: &str) {
    let g = widgets::galley_wrapped(ui, text, theme::semibold(17.0), theme::text(), ui.available_width() - 4.0, 2);
    let (rect, _) = ui.allocate_exact_size(vec2(ui.available_width(), g.size().y + 4.0), Sense::hover());
    ui.painter().galley(pos2(rect.left() + 2.0, rect.top() + 2.0), g, theme::text());
}

/// One of the page's rounded cards (Mochy's StorageCardColumn): its own fill, not the
/// settings glass, so it reads as a card the way Mochy's do.
fn storage_card(ui: &mut Ui, body: impl FnOnce(&mut Ui)) {
    egui::Frame::new()
        .fill(card_fill())
        .corner_radius(CARD_RADIUS)
        .inner_margin(egui::Margin::symmetric(20, 18))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.spacing_mut().item_spacing = vec2(0.0, 0.0);
            body(ui);
        });
}

/// A rounded bar split into parts over `total` (Mochy's UsageBar): each edge eases to
/// where the latest numbers put it, and a part too small to see still gets a sliver,
/// so nothing on the legend is missing.
fn usage_bar(ui: &mut Ui, id: egui::Id, parts: &[(Color32, u64)], total: u64) {
    let (rect, _) = ui.allocate_exact_size(vec2(ui.available_width(), 10.0), Sense::hover());
    let p = ui.painter();
    p.rect_filled(rect, 5.0, track());
    // The bar grows in when the numbers first arrive: Mochy's 1100 ms at the app's
    // own speed, which takes a fraction of whatever it's written with.
    let grow = theme::anim(ui.ctx(), id.with("grow"), u8::from(total > 0) as f32, 1.7);
    let drawn_parts = parts.iter().filter(|(_, b)| *b > 0).count();
    let (mut drawn, mut x) = (0, rect.left());
    for (i, (color, bytes)) in parts.iter().enumerate() {
        if *bytes == 0 {
            continue;
        }
        let frac = if total > 0 { *bytes as f32 / total as f32 } else { 0.0 };
        let eased = theme::anim(ui.ctx(), id.with(i), frac.clamp(0.0, 1.0), 0.4);
        let width = ((eased * rect.width()).max(3.0) * grow).min((rect.right() - x).max(0.0));
        if width <= 0.0 {
            break;
        }
        // The bar's own round ends shape the first and last parts.
        let end = x + width >= rect.right() - 0.5;
        let r = egui::CornerRadius {
            nw: u8::from(drawn == 0) * 5,
            ne: u8::from(drawn + 1 == drawn_parts && end) * 5,
            sw: u8::from(drawn == 0) * 5,
            se: u8::from(drawn + 1 == drawn_parts && end) * 5,
        };
        p.rect_filled(egui::Rect::from_min_max(pos2(x, rect.top()), pos2(x + width, rect.bottom())), r, *color);
        x += width;
        drawn += 1;
    }
}

/// A legend line under a bar: the colour, what it is, and how big it is on the right
/// (Mochy's LegendRow).
fn legend_row(ui: &mut Ui, id: egui::Id, color: Color32, label: &str, size: Option<&str>) {
    let (rect, _) = ui.allocate_exact_size(vec2(ui.available_width(), 20.0), Sense::hover());
    let p = ui.painter();
    p.rect_filled(egui::Rect::from_min_size(pos2(rect.left(), rect.center().y - 10.0), vec2(20.0, 20.0)), 6.0, color);
    let g = p.layout_no_wrap(label.to_owned(), theme::semibold(17.0), theme::text());
    p.galley(pos2(rect.left() + 30.0, rect.center().y - g.size().y / 2.0), g, theme::text());
    if let Some(text) = size {
        value_slide(ui, id, text, rect);
    }
}

/// A number on the right of a legend line, sliding up when a new one replaces it
/// (Mochy's AnimatedContent legend).
fn value_slide(ui: &mut Ui, id: egui::Id, text: &str, row: egui::Rect) {
    let (prev, step) = ui.ctx().data_mut(|d| {
        let e = d.get_temp_mut_or_default::<(String, String, u32)>(id);
        if e.0 != text {
            *e = (text.to_owned(), e.0.clone(), e.2 + 1);
        }
        (e.1.clone(), e.2)
    });
    // How far from the last number to this one, 1 once it has arrived.
    let k = if step == 0 {
        1.0
    } else {
        (ui.ctx().animate_value_with_time(id.with("a"), step as f32, 0.15) - (step - 1) as f32).clamp(0.0, 1.0)
    };
    let p = ui.painter();
    let font = theme::regular(12.0);
    let cur = p.layout_no_wrap(text.to_owned(), font.clone(), theme::with_alpha(muted(), k));
    let top = row.center().y - cur.size().y / 2.0;
    if k < 1.0 {
        // The old number slides up and out, the new one comes in below it.
        let old = p.layout_no_wrap(prev, font, theme::with_alpha(muted(), 1.0 - k));
        p.galley(pos2(row.right() - old.size().x, top - 9.0 * k), old, muted());
    }
    p.galley(pos2(row.right() - cur.size().x, top + 9.0 * (1.0 - k)), cur, muted());
}

/// A full-width pill with its label in the middle, danger ones written in red
/// (Mochy's StorageActionButton). `detail` is a smaller line under the label.
fn action_button(ui: &mut Ui, label: &str, danger: bool, enabled: bool, detail: Option<&str>) -> bool {
    let color = if danger { danger_color() } else { theme::text() };
    let a = if enabled { 1.0 } else { 0.45 };
    // Laid out in the colour it lands in: a galley's own colour is what paints, the
    // paint call's is only a stand-in for uncoloured glyphs.
    let g = ui.painter().layout_no_wrap(label.to_owned(), theme::semibold(15.0), theme::with_alpha(color, a));
    let d = detail.map(|t| ui.painter().layout_no_wrap(t.to_owned(), theme::regular(11.0), theme::with_alpha(muted(), a)));
    let block = g.size().y + d.as_ref().map_or(0.0, |d| d.size().y + 3.0);
    let (rect, resp) = ui.allocate_exact_size(vec2(ui.available_width(), (block + 18.0).max(50.0)), Sense::click());
    let resp = if enabled { widgets::named(resp, label).on_hover_cursor(egui::CursorIcon::PointingHand) } else { resp };
    let h = theme::anim_bool(ui.ctx(), resp.id.with("h"), resp.hovered() && enabled, 0.15);
    let r = rect.shrink(if resp.is_pointer_button_down_on() && enabled { 1.0 } else { 0.0 });
    let p = ui.painter();
    p.rect_filled(r, r.height() / 2.0, card_fill());
    // A hover wash, the desktop part Mochy's touch screen never needed.
    if h > 0.0 {
        p.rect_filled(r, r.height() / 2.0, theme::glass(0.05 * h));
    }
    let top = r.center().y - block / 2.0;
    p.galley(pos2(r.center().x - g.size().x / 2.0, top), g.clone(), color);
    if let Some(d) = d {
        p.galley(pos2(r.center().x - d.size().x / 2.0, top + g.size().y + 3.0), d, muted());
    }
    enabled && resp.clicked()
}

/// Like 1.4%, or <0.1% for a sliver, so a small app never reads as using nothing
/// (Mochy's percentOf).
fn percent_of(part: u64, total: u64) -> String {
    if total == 0 {
        return "0%".to_owned();
    }
    let p = part as f64 * 100.0 / total as f64;
    if p < 0.1 {
        "<0.1%".to_owned()
    } else if p < 10.0 {
        format!("{p:.1}%")
    } else {
        format!("{}%", p as u64)
    }
}
