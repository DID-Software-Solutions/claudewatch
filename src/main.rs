//! claudewatch: GUI showing Claude plan limits (from Anthropic, covers claude.ai + Claude Code)
//! and Claude Code token usage (from ~/.claude/projects/**/*.jsonl). Refreshes every 5s.
#![windows_subsystem = "windows"]
use eframe::egui::{self, Color32, FontFamily, FontId, Pos2, Rect, Sense, Stroke, vec2};
use serde_json::Value;
use std::collections::{BTreeMap, HashSet};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use std::{env, fs, path::Path, path::PathBuf, thread};

const REFRESH: Duration = Duration::from_secs(5);
const USAGE_URL: &str = "https://api.anthropic.com/api/oauth/usage";

// Palette: dusk slate with a sky accent that warms as a limit fills.
const BG: Color32 = Color32::from_rgb(0x1C, 0x24, 0x33);
const PANEL: Color32 = Color32::from_rgb(0x23, 0x2D, 0x40);
const LINE: Color32 = Color32::from_rgb(0x34, 0x40, 0x5A);
const INK: Color32 = Color32::from_rgb(0xE8, 0xEC, 0xF4);
const MUTED: Color32 = Color32::from_rgb(0x8E, 0x9A, 0xB2);
const SKY: Color32 = Color32::from_rgb(0x7C, 0xC4, 0xE4);
const AMBER: Color32 = Color32::from_rgb(0xF0, 0xB4, 0x49);
const RED: Color32 = Color32::from_rgb(0xEF, 0x6F, 0x6C);

fn num(size: f32) -> FontId {
    FontId::new(size, FontFamily::Name("num".into()))
}
fn body(size: f32) -> FontId {
    FontId::proportional(size)
}
fn heat(pct: f32) -> Color32 {
    if pct >= 80.0 {
        RED
    } else if pct >= 50.0 {
        AMBER
    } else {
        SKY
    }
}

#[derive(Default, Clone, Copy)]
struct Tok {
    input: u64,
    output: u64,
    cache_write: u64,
    cache_read: u64,
}

impl Tok {
    fn add(&mut self, o: Tok) {
        self.input += o.input;
        self.output += o.output;
        self.cache_write += o.cache_write;
        self.cache_read += o.cache_read;
    }
    fn total(&self) -> u64 {
        self.input + self.output + self.cache_write + self.cache_read
    }
}

/// One plan limit window, e.g. "five_hour" at 42%.
struct Limit {
    key: String,
    pct: f32,
    resets_at: Option<u64>, // unix seconds
}

#[derive(Default)]
struct Data {
    by_day: BTreeMap<String, Tok>,
    by_model: Vec<(String, Tok)>,
    all: Tok,
    plan: String,
    limits: Vec<Limit>,
    limits_err: Option<String>,
    updated: Option<u64>,
}

fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

/// "2026-11-27T18:00:00..." (UTC) -> unix seconds.
fn parse_utc(s: &str) -> Option<u64> {
    let p = |a: usize, b: usize| s.get(a..b)?.parse::<i64>().ok();
    let (y, m, d) = (p(0, 4)?, p(5, 7)?, p(8, 10)?);
    let (hh, mm, ss) = (p(11, 13)?, p(14, 16)?, p(17, 19)?);
    // Days from civil date (Howard Hinnant's algorithm).
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let doy = (153 * ((m + 9) % 12) + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146_097 + doe - 719_468;
    u64::try_from(days * 86_400 + hh * 3600 + mm * 60 + ss).ok()
}

/// Unix seconds -> "YYYY-MM-DD" (UTC), inverse of the algorithm above.
fn ymd(t: u64) -> String {
    let z = (t / 86_400) as i64 + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    format!("{y:04}-{m:02}-{d:02}")
}

fn jsonl_files(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(rd) = fs::read_dir(dir) else { return };
    for e in rd.flatten() {
        let p = e.path();
        if p.is_dir() {
            jsonl_files(&p, out);
        } else if p.extension().is_some_and(|x| x == "jsonl") {
            out.push(p);
        }
    }
}

fn load_local(root: &Path, d: &mut Data) {
    let mut files = Vec::new();
    jsonl_files(&root.join("projects"), &mut files);

    let mut seen = HashSet::new();
    let mut by_model: BTreeMap<String, Tok> = BTreeMap::new();
    for f in &files {
        let Ok(text) = fs::read_to_string(f) else {
            continue;
        };
        for line in text.lines() {
            let Ok(v) = serde_json::from_str::<Value>(line) else {
                continue;
            };
            let msg = &v["message"];
            let us = &msg["usage"];
            if !us.is_object() {
                continue;
            }
            // Same message is logged once per content block; count it once.
            let key = format!(
                "{}:{}",
                msg["id"].as_str().unwrap_or(""),
                v["requestId"].as_str().unwrap_or("")
            );
            if key != ":" && !seen.insert(key) {
                continue;
            }
            let n = |k: &str| us[k].as_u64().unwrap_or(0);
            let t = Tok {
                input: n("input_tokens"),
                output: n("output_tokens"),
                cache_write: n("cache_creation_input_tokens"),
                cache_read: n("cache_read_input_tokens"),
            };
            let day: String = v["timestamp"]
                .as_str()
                .unwrap_or("unknown")
                .chars()
                .take(10)
                .collect();
            let model = msg["model"].as_str().unwrap_or("unknown").to_string();
            d.by_day.entry(day).or_default().add(t);
            if model != "<synthetic>" {
                by_model.entry(model).or_default().add(t);
            }
            d.all.add(t);
        }
    }
    d.by_model = by_model.into_iter().collect();
    d.by_model
        .sort_by_key(|(_, t)| std::cmp::Reverse(t.total()));
}

/// Claude Code's saved login: ~/.claude/.credentials.json on Windows/Linux, the Keychain on macOS.
fn read_creds(root: &Path) -> Option<String> {
    if let Ok(s) = fs::read_to_string(root.join(".credentials.json")) {
        return Some(s);
    }
    #[cfg(target_os = "macos")]
    {
        let out = std::process::Command::new("security")
            .args([
                "find-generic-password",
                "-s",
                "Claude Code-credentials",
                "-w",
            ])
            .output()
            .ok()?;
        if out.status.success() {
            return String::from_utf8(out.stdout).ok();
        }
    }
    None
}

/// Plan limits from Anthropic, using the login Claude Code stores locally.
/// The token is re-read each time because Claude Code refreshes it.
fn load_limits(root: &Path, d: &mut Data) -> Result<(), String> {
    let creds = read_creds(root).ok_or("Sign in to Claude Code to see plan limits.")?;
    let creds: Value = serde_json::from_str(&creds).map_err(|e| e.to_string())?;
    let oauth = &creds["claudeAiOauth"];
    if let Some(p) = oauth["subscriptionType"].as_str() {
        let mut c = p.chars();
        d.plan = c
            .next()
            .map(|f| f.to_uppercase().collect::<String>() + c.as_str())
            .unwrap_or_default()
            + " plan";
    }
    let token = oauth["accessToken"]
        .as_str()
        .ok_or("Sign in to Claude Code to see plan limits.")?;

    let body = ureq::get(USAGE_URL)
        .header("Authorization", &format!("Bearer {token}"))
        .header("anthropic-beta", "oauth-2025-04-20")
        .call()
        .map_err(|e| match e {
            ureq::Error::StatusCode(401) => {
                "Login expired. Open Claude Code once to renew it.".to_string()
            }
            ureq::Error::StatusCode(429) => {
                "Anthropic is rate limiting requests. Showing the last known values soon."
                    .to_string()
            }
            e => format!("Couldn't reach Anthropic: {e}"),
        })?
        .body_mut()
        .read_to_string()
        .map_err(|e| e.to_string())?;
    let v: Value = serde_json::from_str(&body).map_err(|e| e.to_string())?;

    // Every top-level window that reports a utilization (five_hour, seven_day, ...).
    if let Some(obj) = v.as_object() {
        for (k, w) in obj {
            if let Some(pct) = w["utilization"].as_f64() {
                d.limits.push(Limit {
                    key: k.clone(),
                    pct: pct as f32,
                    resets_at: w["resets_at"].as_str().and_then(parse_utc),
                });
            }
        }
    }
    Ok(())
}

fn limit_name(key: &str) -> String {
    match key {
        "five_hour" => "5-hour session".into(),
        "seven_day" => "Weekly, all models".into(),
        k if k.starts_with("seven_day_") => format!(
            "Weekly, {}",
            k.trim_start_matches("seven_day_").replace('_', " ")
        ),
        k => {
            let s = k.replace('_', " ");
            let mut c = s.chars();
            c.next()
                .map(|f| f.to_uppercase().collect::<String>() + c.as_str())
                .unwrap_or_default()
        }
    }
}

fn until(t: u64) -> String {
    let s = t.saturating_sub(now());
    match s {
        0 => "Resets now".into(),
        s if s < 3600 => format!("Resets in {}m", s / 60),
        s if s < 86_400 => format!("Resets in {}h {}m", s / 3600, s / 60 % 60),
        s => format!("Resets in {}d {}h", s / 86_400, s / 3600 % 24),
    }
}

fn fmt(n: u64) -> String {
    match n {
        n if n >= 1_000_000_000 => format!("{:.2}B", n as f64 / 1e9),
        n if n >= 1_000_000 => format!("{:.1}M", n as f64 / 1e6),
        n if n >= 1_000 => format!("{:.0}K", n as f64 / 1e3),
        n => n.to_string(),
    }
}

fn breakdown(ui: &mut egui::Ui, title: &str, t: &Tok) {
    ui.label(
        egui::RichText::new(title)
            .font(body(13.0))
            .color(INK)
            .strong(),
    );
    egui::Grid::new("bd")
        .num_columns(2)
        .spacing([16.0, 2.0])
        .show(ui, |ui| {
            for (k, n) in [
                ("Input", t.input),
                ("Output", t.output),
                ("Cache write", t.cache_write),
                ("Cache read", t.cache_read),
                ("Total", t.total()),
            ] {
                ui.label(egui::RichText::new(k).color(MUTED));
                ui.label(egui::RichText::new(fmt(n)).font(num(14.0)).color(INK));
                ui.end_row();
            }
        });
}

/// A 270° dial with the percentage in the middle.
fn gauge(ui: &mut egui::Ui, l: &Limit, w: f32) {
    let (rect, _) = ui.allocate_exact_size(vec2(w, 196.0), Sense::hover());
    let p = ui.painter();
    let c = Pos2::new(rect.center().x, rect.top() + 82.0);
    let r = 62.0;
    let arc = |from: f32, to: f32| -> Vec<Pos2> {
        let steps = 64;
        (0..=steps)
            .map(|i| {
                let a = (from + (to - from) * i as f32 / steps as f32).to_radians();
                c + vec2(a.cos(), a.sin()) * r
            })
            .collect()
    };
    let (start, sweep) = (135.0, 270.0);
    // Thick polyline plus a dot on each end for round caps.
    let stroke_arc = |pts: Vec<Pos2>, col: Color32| {
        p.circle_filled(pts[0], 4.5, col);
        p.circle_filled(*pts.last().unwrap(), 4.5, col);
        p.add(egui::Shape::line(pts, Stroke::new(9.0, col)));
    };
    stroke_arc(arc(start, start + sweep), LINE);
    let frac = (l.pct / 100.0).clamp(0.0, 1.0);
    if frac > 0.0 {
        stroke_arc(arc(start, start + sweep * frac.max(0.01)), heat(l.pct));
    }
    p.text(
        c + vec2(0.0, -4.0),
        egui::Align2::CENTER_CENTER,
        format!("{:.0}%", l.pct),
        num(38.0),
        INK,
    );
    p.text(
        c + vec2(0.0, 26.0),
        egui::Align2::CENTER_CENTER,
        "used",
        body(12.0),
        MUTED,
    );
    p.text(
        Pos2::new(c.x, rect.top() + 156.0),
        egui::Align2::CENTER_CENTER,
        limit_name(&l.key),
        body(14.0),
        INK,
    );
    if let Some(t) = l.resets_at {
        p.text(
            Pos2::new(c.x, rect.top() + 176.0),
            egui::Align2::CENTER_CENTER,
            until(t),
            body(12.0),
            MUTED,
        );
    }
}

fn panel(ui: &mut egui::Ui, add: impl FnOnce(&mut egui::Ui)) {
    egui::Frame::new()
        .fill(PANEL)
        .corner_radius(10.0)
        .inner_margin(18.0)
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            add(ui);
        });
}

fn section_title(ui: &mut egui::Ui, text: &str) {
    ui.label(
        egui::RichText::new(text)
            .font(body(15.0))
            .color(INK)
            .strong(),
    );
}

struct App {
    data: Arc<Mutex<Data>>,
    days: usize,
}

impl eframe::App for App {
    fn save(&mut self, storage: &mut dyn eframe::Storage) {
        storage.set_string("days", self.days.to_string());
    }

    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        ui.ctx().request_repaint_after(Duration::from_secs(1)); // keep countdowns current
        let d = self.data.lock().unwrap();
        ui.painter().rect_filled(ui.max_rect(), 0.0, BG);

        egui::ScrollArea::vertical().show(ui, |ui| {
            egui::Frame::new()
                .inner_margin(egui::Margin::symmetric(24, 20))
                .show(ui, |ui| {
                    ui.spacing_mut().item_spacing.y = 14.0;

                    // Header
                    ui.horizontal(|ui| {
                        ui.label(
                            egui::RichText::new("claudewatch")
                                .font(num(24.0))
                                .color(INK),
                        );
                        if !d.plan.is_empty() {
                            ui.label(egui::RichText::new(&d.plan).font(body(13.0)).color(MUTED));
                        }
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            let (txt, dot) = match d.updated {
                                None => ("Loading".to_string(), MUTED),
                                Some(t) => (
                                    format!("Updated {}s ago", now().saturating_sub(t)),
                                    if d.limits_err.is_some() { AMBER } else { SKY },
                                ),
                            };
                            ui.label(egui::RichText::new(txt).font(body(12.0)).color(MUTED));
                            let (r, _) = ui.allocate_exact_size(vec2(8.0, 8.0), Sense::hover());
                            ui.painter().circle_filled(r.center(), 4.0, dot);
                        });
                    });

                    // Plan limits: the one loud element.
                    panel(ui, |ui| {
                        section_title(ui, "Plan limits");
                        ui.label(
                            egui::RichText::new("Shared by claude.ai and Claude Code")
                                .font(body(12.0))
                                .color(MUTED),
                        );
                        if let Some(e) = &d.limits_err {
                            ui.label(egui::RichText::new(e).color(AMBER));
                        }
                        if !d.limits.is_empty() {
                            let w = ui.available_width() / d.limits.len() as f32;
                            ui.horizontal(|ui| {
                                ui.spacing_mut().item_spacing.x = 0.0;
                                for l in &d.limits {
                                    gauge(ui, l, w);
                                }
                            });
                        }
                    });

                    // Daily tokens
                    // Last N calendar days (UTC), empty days included.
                    let zero = Tok::default();
                    let dates: Vec<String> = (0..self.days as u64)
                        .rev()
                        .map(|i| ymd(now() - i * 86_400))
                        .collect();
                    let days: Vec<(&String, &Tok)> = dates
                        .iter()
                        .map(|k| (k, d.by_day.get(k).unwrap_or(&zero)))
                        .collect();
                    let in_range = days.iter().fold(Tok::default(), |mut a, (_, t)| {
                        a.add(**t);
                        a
                    });
                    panel(ui, |ui| {
                        ui.horizontal(|ui| {
                            section_title(ui, "Claude Code tokens");
                            ui.with_layout(
                                egui::Layout::right_to_left(egui::Align::Center),
                                |ui| {
                                    for n in [30, 14, 7] {
                                        let on = self.days == n;
                                        let b = egui::Button::new(
                                            egui::RichText::new(format!("{n} days"))
                                                .font(body(12.0))
                                                .color(if on { BG } else { MUTED }),
                                        )
                                        .fill(if on { SKY } else { PANEL })
                                        .stroke(Stroke::new(1.0, if on { SKY } else { LINE }))
                                        .corner_radius(6.0);
                                        if ui.add(b).clicked() {
                                            self.days = n;
                                        }
                                    }
                                },
                            );
                        });
                        ui.horizontal(|ui| {
                            ui.label(
                                egui::RichText::new(fmt(in_range.total()))
                                    .font(num(30.0))
                                    .color(INK),
                            );
                            ui.label(
                                egui::RichText::new(format!(
                                    "in the last {} days, {} output",
                                    self.days,
                                    fmt(in_range.output)
                                ))
                                .font(body(12.0))
                                .color(MUTED),
                            );
                        });

                        let h = 130.0;
                        let (rect, _) = ui.allocate_exact_size(
                            vec2(ui.available_width(), h + 20.0),
                            Sense::hover(),
                        );
                        let max = days
                            .iter()
                            .map(|(_, t)| t.total())
                            .max()
                            .unwrap_or(1)
                            .max(1) as f32;
                        let slot = rect.width() / days.len().max(1) as f32;
                        let label_every = (days.len() / 7).max(1);
                        let p = ui.painter().clone();
                        p.line_segment(
                            [
                                Pos2::new(rect.left(), rect.top() + h),
                                Pos2::new(rect.right(), rect.top() + h),
                            ],
                            Stroke::new(1.0, LINE),
                        );
                        for (i, (day, t)) in days.iter().enumerate() {
                            let x = rect.left() + i as f32 * slot;
                            let bh = (t.total() as f32 / max * h).max(2.0);
                            let hit = Rect::from_min_max(
                                Pos2::new(x, rect.top()),
                                Pos2::new(x + slot, rect.top() + h),
                            );
                            let bar = Rect::from_min_max(
                                Pos2::new(x + slot * 0.2, rect.top() + h - bh),
                                Pos2::new(x + slot * 0.8, rect.top() + h),
                            );
                            let resp =
                                ui.interact(hit, ui.scope_id().with(("day", i)), Sense::hover());
                            p.rect_filled(bar, 3.0, if resp.hovered() { INK } else { SKY });
                            if i % label_every == 0 || i + 1 == days.len() {
                                p.text(
                                    Pos2::new(x + slot / 2.0, rect.top() + h + 5.0),
                                    egui::Align2::CENTER_TOP,
                                    day.get(5..).unwrap_or(day),
                                    body(11.0),
                                    MUTED,
                                );
                            }
                            resp.on_hover_ui(|ui| breakdown(ui, day, t));
                        }
                    });

                    // Models
                    panel(ui, |ui| {
                        section_title(ui, "By model, all time");
                        let max = d
                            .by_model
                            .first()
                            .map(|(_, t)| t.total())
                            .unwrap_or(1)
                            .max(1) as f32;
                        for (m, t) in &d.by_model {
                            let (rect, resp) = ui.allocate_exact_size(
                                vec2(ui.available_width(), 26.0),
                                Sense::hover(),
                            );
                            let p = ui.painter();
                            let name_w = 190.0;
                            let track = Rect::from_min_max(
                                Pos2::new(rect.left() + name_w, rect.center().y - 4.0),
                                Pos2::new(rect.right() - 70.0, rect.center().y + 4.0),
                            );
                            let fill = Rect::from_min_size(
                                track.min,
                                vec2(
                                    (track.width() * t.total() as f32 / max).max(3.0),
                                    track.height(),
                                ),
                            );
                            p.text(
                                Pos2::new(rect.left(), rect.center().y),
                                egui::Align2::LEFT_CENTER,
                                m.trim_start_matches("claude-"),
                                body(13.0),
                                if resp.hovered() { INK } else { MUTED },
                            );
                            p.rect_filled(track, 4.0, LINE);
                            p.rect_filled(fill, 4.0, SKY);
                            p.text(
                                Pos2::new(rect.right(), rect.center().y),
                                egui::Align2::RIGHT_CENTER,
                                fmt(t.total()),
                                num(15.0),
                                INK,
                            );
                            resp.on_hover_ui(|ui| breakdown(ui, m, t));
                        }
                        ui.label(
                            egui::RichText::new(format!(
                                "{} tokens total, from the logs Claude Code keeps on this computer",
                                fmt(d.all.total())
                            ))
                            .font(body(12.0))
                            .color(MUTED),
                        );
                    });
                });
        });
    }
}

fn setup_style(ctx: &egui::Context) {
    let mut fonts = egui::FontDefinitions::default();
    for (name, file, family) in [
        ("segoe", "SegUIVar.ttf", FontFamily::Proportional),
        ("bahn", "bahnschrift.ttf", FontFamily::Name("num".into())),
    ] {
        let fam = fonts.families.entry(family).or_default();
        if let Ok(bytes) = fs::read(format!("C:\\Windows\\Fonts\\{file}")) {
            fonts
                .font_data
                .insert(name.into(), Arc::new(egui::FontData::from_owned(bytes)));
            fam.insert(0, name.into());
        }
    }
    // "num" falls back to the default font when Bahnschrift is missing.
    let fallback = fonts.families[&FontFamily::Proportional].clone();
    fonts
        .families
        .get_mut(&FontFamily::Name("num".into()))
        .unwrap()
        .extend(fallback);
    ctx.set_fonts(fonts);

    let mut v = egui::Visuals::dark();
    v.panel_fill = BG;
    v.window_fill = PANEL;
    v.window_stroke = Stroke::new(1.0, LINE);
    v.override_text_color = Some(INK);
    v.widgets.hovered.weak_bg_fill = LINE;
    ctx.set_visuals(v);
}

fn main() -> eframe::Result {
    let home = env::var("USERPROFILE")
        .or_else(|_| env::var("HOME"))
        .expect("no home dir");
    let root = Path::new(&home).join(".claude");
    let data = Arc::new(Mutex::new(Data::default()));

    let opts = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([760.0, 860.0])
            .with_min_inner_size([520.0, 500.0])
            .with_title("claudewatch"),
        ..Default::default()
    };
    eframe::run_native(
        "claudewatch",
        opts,
        Box::new(move |cc| {
            setup_style(&cc.egui_ctx);
            let days = cc
                .storage
                .and_then(|s| s.get_string("days"))
                .and_then(|s| s.parse().ok())
                .unwrap_or(14);
            let ctx = cc.egui_ctx.clone();
            let shared = data.clone();
            // Refresh worker: reload everything off the UI thread, then repaint.
            thread::spawn(move || {
                loop {
                    let mut d = Data::default();
                    load_local(&root, &mut d);
                    if let Err(e) = load_limits(&root, &mut d) {
                        d.limits_err = Some(e);
                    }
                    d.updated = Some(now());
                    *shared.lock().unwrap() = d;
                    ctx.request_repaint();
                    thread::sleep(REFRESH);
                }
            });
            Ok(Box::new(App { data, days }))
        }),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn utc_round_trip() {
        let t = parse_utc("2026-11-27T18:05:09.123+00:00").unwrap();
        assert_eq!(t, 1_795_802_709);
        assert_eq!(ymd(t), "2026-11-27");
        assert_eq!(
            ymd(parse_utc("2024-02-29T00:00:00Z").unwrap()),
            "2024-02-29"
        );
        assert_eq!(parse_utc("garbage"), None);
    }

    #[test]
    fn formats_token_counts() {
        assert_eq!(fmt(999), "999");
        assert_eq!(fmt(12_400), "12K");
        assert_eq!(fmt(3_450_000), "3.5M");
        assert_eq!(fmt(2_000_000_000), "2.00B");
    }

    #[test]
    fn names_limits() {
        assert_eq!(limit_name("five_hour"), "5-hour session");
        assert_eq!(limit_name("seven_day"), "Weekly, all models");
        assert_eq!(limit_name("seven_day_opus"), "Weekly, opus");
        assert_eq!(limit_name("extra_thing"), "Extra thing");
    }

    #[test]
    fn sums_tokens() {
        let mut a = Tok {
            input: 1,
            output: 2,
            cache_write: 3,
            cache_read: 4,
        };
        a.add(a);
        assert_eq!(a.total(), 20);
    }
}
