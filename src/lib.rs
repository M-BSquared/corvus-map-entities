//! Map Entities: a tactical layer on the Home map
//! (`python/plugins/map-entities/map-entities.js` and `.css`).
//!
//! Things the operator places by hand at an exact coordinate: gates (two
//! posts and the opening between them), trees (a crown radius and a height)
//! and a dozen symbols with an optional radius, drawn as a light area (a
//! radar's reach, a blast's danger zone), a full circle or a sector. The
//! plugin watches the aircraft's track and says whether it flew through a
//! gate's opening, over it or under it, and whether it entered an area.
//!
//! What is placed is saved through `save_settings`; what the aircraft did is
//! kept for the session only, and "Reset results" starts it again.

pub mod mcp;
pub mod model;
mod view;

use std::collections::{HashMap, HashSet};

use serde_json::{json, Value};

use self::model::{
    bearing_deg, clean_path, make_entity, normalize_list, points_of, process_step, sample_of, sector_ring,
    status_color, Entity, Results, Sample, MAX_ENTITIES, MAX_STEP_M, MEDIA_FILES, MIN_STEP_M,
};
use corvus_api::i18n::{tr as translate, tr_fmt as translate_fmt};
use corvus_api::plugin::options::OptionSpec;
use corvus_api::plugin::shapes::{self, circle_ring, Style};
use corvus_api::plugin::{FolderScan, Host, Info, MapEvent, McpTool, Plugin, Reply};
use corvus_api::ui::filepicker::{self, Filter};

const UNDO_SECS: f64 = 8.0;
const PENDING_KEY: &str = "pending";
/// The gear's switch for placing the pictures of a folder without any AI client.
const OPT_IMPORT: &str = "photoImport";
/// The one folder read this plugin asks for.
const IMPORT_TAG: u64 = 1;
/// What this plugin offers other plugins while "Share with other plugins" is on:
/// the entities as a list, read as `map-entities/shared`.
pub const SHARED_TOPIC: &str = "shared";

/// The switches of the plugin's gear.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Opts {
    pub labels: bool,
    pub notify: bool,
    pub armed_only: bool,
    pub share: bool,
    pub import: bool,
}

impl Default for Opts {
    fn default() -> Opts {
        Opts {
            labels: true,
            notify: true,
            armed_only: true,
            share: false,
            import: false,
        }
    }
}

/// A kind being placed: the next click on the map makes one.
#[derive(Debug, Clone, PartialEq)]
pub struct Placing {
    pub kind: &'static str,
    pub step: u8,
    pub first: Option<[f64; 2]>,
}

/// A click on the map that moves or aims one entity.
#[derive(Debug, Clone, PartialEq)]
pub enum Aim {
    Post(String, char),
    Bearing(String),
}

/// What the last read of a folder placed, and what it says about it.
pub struct Imported {
    pub ids: Vec<String>,
    pub said: String,
}

/// What a delete can still take back.
pub struct Undo {
    pub items: Vec<(Entity, usize)>,
    pub until: f64,
}

#[derive(Default)]
pub struct MapEntities {
    pub entities: Vec<Entity>,
    pub results: HashMap<String, Results>,
    pub selected: Option<String>,
    pub placing: Option<Placing>,
    aim: Option<Aim>,
    pub undo: Option<Undo>,
    /// A folder is being picked or read.
    pub importing: bool,
    pub imported: Option<Imported>,
    prev: Option<Sample>,
    pub opts: Opts,
    started: bool,
    pub open: bool,
    pub(crate) mcp: mcp::State,
}

impl Plugin for MapEntities {
    fn info(&self) -> Info {
        Info {
            id: "map-entities",
            name: "Map Entities",
            icon: "map-pin",
            description: "Gates, trees, people and devices with a range, placed exactly on the Home map",
            version: "2.0.0",
            order: 35,
            can_tab: true,
        }
    }

    fn options(&self) -> Vec<OptionSpec> {
        vec![
            OptionSpec::toggle("labels", "Show names on the map", true),
            OptionSpec::toggle("notify", "Notify on gate passes and area entries", true),
            OptionSpec::toggle("armedOnly", "Only check while armed", true),
            OptionSpec::toggle("share", "Share with other plugins", false).hint(
                "Lets another plugin, such as TAK, read what is placed here. Nothing is shared while this is off, and this plugin works the same without any other.",
            ),
            OptionSpec::toggle(OPT_IMPORT, "Import photos from a folder", false).hint(
                "Adds a button that reads the position and time in every picture and video of a folder you choose and places a file mark for each, without any AI client. It needs files that were saved with a GPS position. Videos need ffmpeg.",
            ),
            OptionSpec::toggle(mcp::MCP_READ, "AI clients may read what is placed", false).hint(
                "Through Corvus' MCP server an AI client can list what is placed, with what the aircraft did to it, and show it on the map. It also needs Plugin tools switched on in Settings, Connections, MCP.",
            ),
            OptionSpec::toggle(mcp::MCP_EDIT, "AI clients may place things", false).hint(
                "Through Corvus' MCP server an AI client can place things on the map, and change or remove the ones it added. That includes file markers with the path of a picture or video, which opens when you double click it. What you placed stays yours, what it added is marked AI and is not shared with other plugins. Nothing of it reaches the aircraft.",
            ),
        ]
    }

    fn start(&mut self, h: &mut Host) {
        if self.started {
            return;
        }
        self.started = true;
        self.entities = normalize_list(h.settings().get("entities").unwrap_or(&Value::Null));
        self.read_options(h);
        self.draw_all(h);
        self.share(h);
    }

    fn mcp_tools(&self, h: &Host) -> Vec<McpTool> {
        Self::mcp_tools_for(h)
    }

    fn mcp_call(&mut self, name: &str, args: &Value, token: u64, h: &mut Host) {
        self.mcp_start(name, args, token, h);
    }

    fn mcp_take(&mut self, token: u64) -> Option<Result<Value, String>> {
        self.mcp.take(token)
    }

    fn tick(&mut self, h: &mut Host) {
        self.mcp_run();
        if let Some(u) = &self.undo {
            if h.now >= u.until {
                self.undo = None;
            } else {
                h.ctx.request_repaint_after(std::time::Duration::from_millis(500));
            }
        }
        self.on_state(h);
    }

    fn open(&mut self, _h: &mut Host) {
        self.open = true;
    }

    fn close(&mut self, h: &mut Host) {
        self.open = false;
        self.stop_placing(h);
        self.aim = None;
        self.undo = None;
        self.imported = None;
    }

    fn media_scanned(&mut self, tag: u64, scan: Option<FolderScan>, h: &mut Host) {
        if tag == IMPORT_TAG {
            self.importing = false;
            if let Some(scan) = scan {
                self.take_scan(scan, h);
            }
        }
    }

    fn options_changed(&mut self, h: &mut Host) {
        self.read_options(h);
        self.draw_all(h);
        self.share(h);
    }

    fn map_event(&mut self, e: MapEvent, h: &mut Host) {
        match e {
            MapEvent::Click(key) => {
                if let Some(id) = entity_of_key(&key) {
                    self.select(Some(id), h);
                }
            }
            MapEvent::DoubleClick(key) => {
                if let Some(id) = entity_of_key(&key) {
                    self.select(Some(id.clone()), h);
                    self.open_file(&id, h);
                }
            }
            MapEvent::Drag(key, at) => self.drag_preview(&key, at, h),
            MapEvent::DragEnd(key, at) => self.drag_end(&key, at, h),
            MapEvent::Picked(at) => self.picked(at, h),
            MapEvent::PickCancelled => {
                self.placing = None;
                self.aim = None;
                h.map().remove(PENDING_KEY);
            }
        }
    }

    fn message(&mut self, _from: &str, msg: &Value, h: &mut Host) {
        if msg.get("op").and_then(Value::as_str) == Some("add") {
            self.add_shared(msg, h);
        }
    }

    fn show(&mut self, ui: &mut corvus_api::egui::Ui, area: corvus_api::egui::Rect, h: &mut Host) -> f32 {
        view::show(self, ui, area, h)
    }
}

/// How large the symbol of an entity is on the map. File markers are the smallest: the
/// pictures of one flight lie close together.
fn marker_size(e: &Entity) -> f32 {
    if e.is_file() {
        20.0
    } else if e.is_tree() {
        24.0
    } else {
        28.0
    }
}

/// `moveHandler`'s id: a shape's key is `<id>-<l|a|b|m|c>`.
fn entity_of_key(key: &str) -> Option<String> {
    let (id, suffix) = key.rsplit_once('-')?;
    matches!(suffix, "l" | "a" | "b" | "m" | "c").then(|| id.to_string())
}

impl MapEntities {
    fn read_options(&mut self, h: &Host) {
        let o = h.options();
        let on = |k: &str| o.get(k).and_then(Value::as_bool) != Some(false);
        self.opts = Opts {
            labels: on("labels"),
            notify: on("notify"),
            armed_only: on("armedOnly"),
            share: o.get("share").and_then(Value::as_bool) == Some(true),
            import: o.get(OPT_IMPORT).and_then(Value::as_bool) == Some(true),
        };
        if !self.opts.import {
            self.imported = None;
        }
    }

    pub fn find(&self, id: &str) -> Option<&Entity> {
        self.entities.iter().find(|e| e.id == id)
    }

    fn find_mut(&mut self, id: &str) -> Option<&mut Entity> {
        self.entities.iter_mut().find(|e| e.id == id)
    }

    // ---- watching the aircraft ------------------------------------------------

    fn on_state(&mut self, h: &mut Host) {
        let Some(cur) = sample_of(h.state, self.opts.armed_only) else {
            self.prev = None;
            return;
        };
        let Some(prev) = self.prev else {
            self.prev = Some(cur);
            return;
        };
        let step = model::distance_m([prev.lng, prev.lat], [cur.lng, cur.lat]);
        if step > MAX_STEP_M {
            self.prev = Some(cur);
            return;
        }
        if step < MIN_STEP_M && (cur.alt - prev.alt).abs() < MIN_STEP_M {
            return;
        }
        let events = process_step(&self.entities, &mut self.results, prev, cur, h.now);
        self.prev = Some(cur);
        for ev in &events {
            if self.opts.notify {
                h.notify(ev.level, &ev.text);
            }
            if let Some(e) = self.find(&ev.entity).cloned() {
                self.draw_entity(&e, h);
            }
        }
    }

    pub fn reset_results(&mut self, h: &mut Host) {
        self.results.clear();
        self.draw_all(h);
    }

    // ---- drawing on the map ---------------------------------------------------

    fn undraw(&self, id: &str, h: &mut Host) {
        for s in ["l", "a", "b", "m", "c"] {
            h.map().remove(&format!("{id}-{s}"));
        }
    }

    pub fn draw_all(&self, h: &mut Host) {
        for e in &self.entities {
            self.draw_entity(e, h);
        }
    }

    pub fn draw_entity(&self, e: &Entity, h: &mut Host) {
        let color = shapes::hex(status_color(e, self.results.get(&e.id))).unwrap_or(shapes::DEFAULT_COLOR);
        let selected = self.selected.as_deref() == Some(e.id.as_str());
        let label = if self.opts.labels { e.name.as_str() } else { "" };
        let id = &e.id;
        let style = Style::color(color);
        let m = h.map();
        if e.is_gate() {
            m.remove(&format!("{id}-c"));
            m.line(
                &format!("{id}-l"),
                &[[e.lng, e.lat], [e.lng2, e.lat2]],
                style,
                Some(5.0),
                false,
            );
            for (suffix, at, which) in [("a", [e.lng, e.lat], "post A"), ("b", [e.lng2, e.lat2], "post B")] {
                m.marker(
                    &format!("{id}-{suffix}"),
                    at,
                    "",
                    "",
                    &format!("{}, {which}", e.name),
                    style,
                    Some(18.0),
                    selected,
                    selected,
                    true,
                );
            }
            m.marker(
                &format!("{id}-m"),
                [(e.lng + e.lng2) / 2.0, (e.lat + e.lat2) / 2.0],
                e.kind.id,
                label,
                &e.name,
                style,
                None,
                selected,
                false,
                true,
            );
            return;
        }
        for s in ["l", "a", "b"] {
            m.remove(&format!("{id}-{s}"));
        }
        let key = format!("{id}-c");
        if e.radius > 0.0 {
            let st = style.opacity(1.0);
            let dashed = !e.is_tree();
            let ring = if e.sector_angle() < 360.0 {
                sector_ring(e)
            } else {
                circle_ring([e.lng, e.lat], e.radius, None)
            };
            m.polygon(&key, &ring, st, Some(0.14), Some(2.5), dashed);
        } else {
            m.remove(&key);
        }
        let title = if e.path.is_empty() {
            e.name.clone()
        } else {
            translate_fmt("{name}: double click to open the file", &[("name", &e.name)])
        };
        m.marker(
            &format!("{id}-m"),
            [e.lng, e.lat],
            e.kind.id,
            label,
            &title,
            style,
            Some(marker_size(e)),
            selected,
            selected,
            true,
        );
    }

    /// A gate's post being dragged: the line follows it.
    fn drag_preview(&mut self, key: &str, at: [f64; 2], h: &mut Host) {
        let Some(id) = entity_of_key(key) else { return };
        let Some(e) = self.find(&id).cloned() else { return };
        if !e.is_gate() {
            return;
        }
        let color = shapes::hex(status_color(&e, self.results.get(&id))).unwrap_or(shapes::DEFAULT_COLOR);
        let (a, b) = if key.ends_with("-a") {
            (at, [e.lng2, e.lat2])
        } else {
            ([e.lng, e.lat], at)
        };
        h.map()
            .line(&format!("{id}-l"), &[a, b], Style::color(color), Some(5.0), false);
    }

    fn drag_end(&mut self, key: &str, at: [f64; 2], h: &mut Host) {
        let Some(id) = entity_of_key(key) else { return };
        let which = if key.ends_with("-b") { 'b' } else { 'a' };
        self.move_post(&id, which, at, h);
    }

    fn move_post(&mut self, id: &str, which: char, at: [f64; 2], h: &mut Host) {
        if let Some(e) = self.find_mut(id) {
            if which == 'b' {
                e.lng2 = at[0];
                e.lat2 = at[1];
            } else {
                e.lng = at[0];
                e.lat = at[1];
            }
        }
        if let Some(e) = self.find(id).cloned() {
            self.draw_entity(&e, h);
        }
        self.save(h);
    }

    // ---- saving ---------------------------------------------------------------

    pub fn save(&self, h: &mut Host) {
        let _ = self.save_reply(h);
    }

    fn save_reply(&self, h: &mut Host) -> Reply {
        let list: Vec<Value> = self.entities.iter().map(Entity::to_json).collect();
        let reply = h.save_settings(&json!({"entities": list}), false);
        self.share(h);
        reply
    }

    /// Offers the entities to other plugins while the option is on, and takes them
    /// back when it is off. Not what an AI client added: another plugin may send it on.
    fn share(&self, h: &mut Host) {
        if self.opts.share {
            let list: Vec<Value> = self
                .entities
                .iter()
                .filter(|e| !e.by_ai)
                .map(Entity::shared_json)
                .collect();
            h.publish(SHARED_TOPIC, Value::Array(list));
        } else {
            h.retract(SHARED_TOPIC);
        }
    }

    /// Another plugin asked for a point: it is added like one the operator placed.
    fn add_shared(&mut self, msg: &Value, h: &mut Host) {
        match model::entity_from_message(msg, &self.entities) {
            Some(e) if self.entities.len() < MAX_ENTITIES => {
                let name = e.name.clone();
                self.add(e, h);
                h.notify(
                    "info",
                    &translate_fmt("Added {name} to Map Entities.", &[("name", &name)]),
                );
            }
            Some(_) => h.notify("warning", &translate("Map Entities is full, nothing was added.")),
            None => h.notify("warning", &translate("Map Entities could not place that point.")),
        }
    }

    // ---- files ----------------------------------------------------------------

    /// Shows a file entity's picture or video in a card of the program. Any other
    /// kind has nothing to open.
    pub fn open_file(&mut self, id: &str, h: &mut Host) {
        let Some(e) = self.find(id).filter(|e| e.is_file()) else {
            return;
        };
        if e.path.is_empty() {
            let say = translate_fmt(
                "{name} has no file yet. Choose one for it in Map Entities.",
                &[("name", &e.name)],
            );
            h.notify("info", &say);
        } else {
            h.show_file(&e.path, &e.name);
        }
    }

    /// Asks the operator for the picture or video of a file entity.
    pub fn choose_file(&mut self, id: &str, h: &mut Host) {
        let Some(current) = self.find(id).map(|e| e.path.clone()) else {
            return;
        };
        let filter = Filter {
            name: "Pictures and videos",
            extensions: &MEDIA_FILES,
        };
        let start = filepicker::start_folder(&current);
        if let Some(path) = filepicker::pick_file("Choose a picture or video", Some(filter), start.as_deref()) {
            let path = clean_path(&path.to_string_lossy());
            self.update(id, h, |e| e.path = path);
        }
    }

    // ---- the pictures of a folder ---------------------------------------------

    /// Has the operator pick a folder; the program reads where its pictures were taken.
    pub fn start_import(&mut self, h: &mut Host) {
        if self.importing || !self.opts.import {
            return;
        }
        self.importing = true;
        self.imported = None;
        h.scan_media_folder(IMPORT_TAG);
    }

    /// Places a file mark for each file of a read folder that is not on the map yet,
    /// and says what came of it.
    fn take_scan(&mut self, scan: FolderScan, h: &mut Host) {
        if let Some(why) = scan.error {
            self.imported = Some(Imported {
                ids: Vec::new(),
                said: translate(&why).into_owned(),
            });
            return;
        }
        let mut there: HashSet<String> = self
            .entities
            .iter()
            .filter(|e| e.is_file())
            .map(|e| e.path.clone())
            .collect();
        let (mut already, mut full, mut unplaced) = (0u32, 0u32, scan.without_place);
        let first = self.entities.len();
        for found in &scan.files {
            if there.contains(&clean_path(&found.path)) {
                already += 1;
            } else if self.entities.len() >= MAX_ENTITIES {
                full += 1;
            } else if let Some(e) = model::entity_from_scan(found, &self.entities) {
                there.insert(e.path.clone());
                self.entities.push(e);
            } else {
                unplaced += 1;
            }
        }
        let new = &self.entities[first..];
        let ids: Vec<String> = new.iter().map(|e| e.id.clone()).collect();
        let points: Vec<[f64; 2]> = new.iter().flat_map(points_of).collect();
        let count = |text: &str, n: u32| (n > 0).then(|| translate_fmt(text, &[("n", &n.to_string())]));
        let mut said = vec![translate_fmt("Files placed: {n}", &[("n", &ids.len().to_string())])];
        said.extend(
            [
                count("Already on the map: {n}", already),
                count("Without a position: {n}", unplaced),
                count("Videos not read, ffmpeg is not installed: {n}", scan.videos_unread),
                count("Left out, the map is full: {n}", full),
                scan.cut
                    .then(|| translate("The folder holds more files than were read.").into_owned()),
            ]
            .into_iter()
            .flatten(),
        );
        if !ids.is_empty() {
            self.draw_all(h);
            self.save(h);
            h.fit(points);
        }
        self.imported = Some(Imported {
            ids,
            said: said.join("\n"),
        });
    }

    /// Takes back what the last folder placed; the delete can be undone as any other.
    pub fn remove_imported(&mut self, h: &mut Host) {
        let Some(imported) = self.imported.take() else { return };
        let ids: HashSet<String> = imported.ids.into_iter().collect();
        let mut gone = Vec::new();
        let mut kept = Vec::new();
        for (i, e) in std::mem::take(&mut self.entities).into_iter().enumerate() {
            if ids.contains(&e.id) {
                gone.push((e, i));
            } else {
                kept.push(e);
            }
        }
        self.entities = kept;
        for (e, _) in &gone {
            self.undraw(&e.id, h);
        }
        if self.selected.as_ref().is_some_and(|id| ids.contains(id)) {
            self.selected = None;
        }
        if !gone.is_empty() {
            self.save(h);
            self.remember(gone, h);
        }
    }

    // ---- editing --------------------------------------------------------------

    pub fn select(&mut self, id: Option<String>, h: &mut Host) {
        let before = self.selected.clone();
        self.selected = id.filter(|i| self.find(i).is_some());
        for x in [before, self.selected.clone()].into_iter().flatten() {
            if let Some(e) = self.find(&x).cloned() {
                self.draw_entity(&e, h);
            }
        }
    }

    pub fn add(&mut self, e: Entity, h: &mut Host) -> bool {
        if self.entities.len() >= MAX_ENTITIES {
            return false;
        }
        self.selected = Some(e.id.clone());
        self.entities.push(e);
        self.draw_all(h);
        self.save(h);
        true
    }

    fn remember(&mut self, items: Vec<(Entity, usize)>, h: &Host) {
        self.undo = Some(Undo {
            items,
            until: h.now + UNDO_SECS,
        });
    }

    pub fn remove(&mut self, id: &str, h: &mut Host) {
        let Some(i) = self.entities.iter().position(|e| e.id == id) else {
            return;
        };
        let e = self.entities.remove(i);
        self.results.remove(id);
        self.undraw(id, h);
        if self.selected.as_deref() == Some(id) {
            self.selected = None;
        }
        self.save(h);
        self.remember(vec![(e, i)], h);
    }

    pub fn remove_all(&mut self, h: &mut Host) {
        if self.entities.is_empty() {
            return;
        }
        let items: Vec<(Entity, usize)> = std::mem::take(&mut self.entities)
            .into_iter()
            .enumerate()
            .map(|(i, e)| (e, i))
            .collect();
        for (e, _) in &items {
            self.undraw(&e.id, h);
        }
        self.results.clear();
        self.selected = None;
        self.save(h);
        self.remember(items, h);
    }

    pub fn undo_delete(&mut self, h: &mut Host) {
        let Some(u) = self.undo.take() else { return };
        let mut items = u.items;
        items.sort_by_key(|i| i.1);
        for (e, index) in items {
            self.selected = Some(e.id.clone());
            let at = index.min(self.entities.len());
            self.entities.insert(at, e);
        }
        self.draw_all(h);
        self.save(h);
    }

    /// Changes one entity and redraws and saves it.
    pub fn update(&mut self, id: &str, h: &mut Host, f: impl FnOnce(&mut Entity)) {
        if let Some(e) = self.find_mut(id) {
            f(e);
        }
        if let Some(e) = self.find(id).cloned() {
            self.draw_entity(&e, h);
        }
        self.save(h);
    }

    // ---- placing --------------------------------------------------------------

    pub fn stop_placing(&mut self, h: &mut Host) {
        if self.placing.take().is_some() {
            h.cancel_pick();
        }
        h.map().remove(PENDING_KEY);
    }

    pub fn start_placing(&mut self, kind: &'static str, h: &mut Host) {
        self.stop_placing(h);
        self.aim = None;
        self.placing = Some(Placing {
            kind,
            step: 0,
            first: None,
        });
        h.pick_point();
    }

    /// The arrow beside a coordinate or the direction: one click moves it.
    pub fn aim_at(&mut self, aim: Aim, h: &mut Host) {
        self.stop_placing(h);
        self.aim = Some(aim);
        h.pick_point();
    }

    fn picked(&mut self, at: [f64; 2], h: &mut Host) {
        if let Some(aim) = self.aim.take() {
            match aim {
                Aim::Post(id, which) => {
                    self.update(&id, h, |e| {
                        if which == 'b' {
                            e.lng2 = at[0];
                            e.lat2 = at[1];
                        } else {
                            e.lng = at[0];
                            e.lat = at[1];
                        }
                    });
                }
                Aim::Bearing(id) => {
                    if let Some(e) = self.find(&id) {
                        let b = (bearing_deg([e.lng, e.lat], at).round() as i64 % 360) as f64;
                        self.update(&id, h, |e| e.bearing = b);
                    }
                }
            }
            return;
        }
        let Some(p) = self.placing.clone() else { return };
        let kind = model::kind_of(p.kind).expect("a kind being placed exists");
        if kind.gate && p.step == 0 {
            self.placing = Some(Placing {
                step: 1,
                first: Some(at),
                ..p
            });
            if let Some(c) = shapes::hex(kind.color) {
                h.map().marker(
                    PENDING_KEY,
                    at,
                    "",
                    "",
                    "",
                    Style::color(c),
                    Some(18.0),
                    false,
                    false,
                    false,
                );
            }
            h.pick_point();
            return;
        }
        let e = make_entity(p.kind, p.first.unwrap_or(at), p.first.map(|_| at), &self.entities);
        h.map().remove(PENDING_KEY);
        self.placing = Some(Placing {
            step: 0,
            first: None,
            ..p
        });
        if let Some(e) = e {
            self.add(e, h);
        }
        h.pick_point();
    }

    /// The corners to frame everything on the map.
    pub fn all_points(&self) -> Vec<[f64; 2]> {
        self.entities.iter().flat_map(points_of).collect()
    }
}

/// The language files of this plugin, for the translation layer.
pub const LANG: &[(&str, &str)] = &[
    ("de", include_str!("../lang/de.json")),
    ("fr", include_str!("../lang/fr.json")),
    ("dl", include_str!("../lang/dl.json")),
];

/// One instance of the plugin, for the registry.
pub fn plugin() -> Box<dyn Plugin> {
    Box::new(MapEntities::default())
}

corvus_api::export_plugin!(plugin, LANG);

#[cfg(test)]
mod tests {
    #[test]
    fn the_manifest_says_what_the_plugin_says_of_itself() {
        let manifest: serde_json::Value = serde_json::from_str(include_str!("../plugin.json")).expect("plugin.json");
        let i = MapEntities::default().info();
        for (key, want) in [
            ("id", i.id),
            ("name", i.name),
            ("description", i.description),
            ("icon", i.icon),
            ("version", i.version),
        ] {
            assert_eq!(
                manifest[key], want,
                "plugin.json says something else than the plugin for {key}"
            );
        }
        assert_eq!(manifest["order"], i.order);
        assert_eq!(manifest["can_tab"], i.can_tab);
    }

    use super::*;

    #[test]
    fn a_shapes_key_names_its_entity() {
        assert_eq!(entity_of_key("e1abc-m"), Some("e1abc".into()));
        assert_eq!(entity_of_key("my-id_1-a"), Some("my-id_1".into()));
        assert_eq!(entity_of_key("pending"), None);
        assert_eq!(entity_of_key("x-z"), None);
    }

    #[test]
    fn a_second_import_adds_nothing_twice_and_the_last_one_can_be_taken_back() {
        use corvus_api::plugin::bus::Bus;
        use corvus_api::plugin::shapes::ShapeSet;
        use corvus_api::plugin::{Outbox, Placed};
        let ctx = corvus_api::egui::Context::default();
        let server = corvus_api::net::http::Server::local(9);
        let (state, mut saved, options) = (json!({}), json!({}), serde_json::Map::new());
        let (mut shapes, mut out, mut bus) = (ShapeSet::new("map-entities"), Outbox::default(), Bus::default());
        let info = MapEntities::default().info();
        let mut h = Host::new(
            &ctx,
            &server,
            &state,
            info,
            &mut shapes,
            &mut saved,
            &options,
            &mut out,
            &mut bus,
        );
        let mut p = MapEntities::default();
        p.opts.import = true;
        let found = |path: &str| Placed {
            path: path.into(),
            lat: 48.0,
            lng: 11.0,
            ..Placed::default()
        };
        p.add(model::make_entity("tree", [11.0, 48.0], None, &[]).unwrap(), &mut h);
        p.start_import(&mut h);
        assert!(p.importing);
        let scan = |files: &[&str]| {
            Some(FolderScan {
                files: files.iter().map(|f| found(f)).collect(),
                without_place: 2,
                ..FolderScan::default()
            })
        };
        p.media_scanned(IMPORT_TAG, scan(&["/c/a.jpg", "/c/b.jpg", "/c/a.jpg"]), &mut h);
        assert!(!p.importing);
        assert_eq!(p.entities.len(), 3);
        let said = &p.imported.as_ref().unwrap().said;
        assert_eq!(said, "Files placed: 2\nAlready on the map: 1\nWithout a position: 2");

        p.media_scanned(IMPORT_TAG, scan(&["/c/b.jpg", "/c/c.jpg"]), &mut h);
        assert_eq!(p.entities.len(), 4);
        p.remove_imported(&mut h);
        let names: Vec<&str> = p.entities.iter().map(|e| e.name.as_str()).collect();
        assert_eq!(names, ["Tree 1", "a.jpg", "b.jpg"], "only the last import goes");
        p.undo_delete(&mut h);
        assert_eq!(p.entities.len(), 4);

        // A folder given up, and one that cannot be read.
        p.start_import(&mut h);
        p.media_scanned(IMPORT_TAG, None, &mut h);
        assert!(!p.importing && p.imported.is_none());
        let broken = FolderScan {
            error: Some("The folder cannot be read.".into()),
            ..FolderScan::default()
        };
        p.media_scanned(IMPORT_TAG, Some(broken), &mut h);
        assert!(p.imported.as_ref().is_some_and(|i| i.ids.is_empty()));
    }

    #[test]
    fn a_file_marker_is_smaller_than_every_other_symbol() {
        let size = |kind| marker_size(&model::make_entity(kind, [11.0, 48.0], None, &[]).unwrap());
        for kind in model::KINDS.iter().filter(|k| !k.gate && k.id != model::FILE_KIND) {
            assert!(size(model::FILE_KIND) < size(kind.id), "{}", kind.id);
        }
    }

    #[test]
    fn the_options_default_to_on() {
        let o = Opts::default();
        assert!(o.labels && o.notify && o.armed_only);
    }
}
