//! What Map Entities offers an AI client through Corvus' MCP server, when the
//! operator has allowed it twice: Plugin tools in Settings, Connections, MCP,
//! and one or both of the switches in this plugin's gear.
//!
//! "May read" lists what is placed with what the aircraft did to it, and frames
//! it on the map. "May place" adds entities, and changes or removes the ones an
//! AI client added: what the operator placed stays the operator's. Nothing here
//! reaches the aircraft, and what an AI client added is not shared with other
//! plugins, so it does not go out over TAK.

use corvus_api::plugin::{Host, McpTool, Reply};
use serde_json::{json, Map, Value};

use super::model::{media_path, normalize_entity, points_of, Entity, Results, FILE_KIND, KINDS, MAX_ENTITIES};
use super::MapEntities;

pub const MCP_READ: &str = "mcp_read";
pub const MCP_EDIT: &str = "mcp_edit";
/// So a client that does not stop leaves room for the operator. Enough for the
/// pictures of a flight.
pub const MAX_BY_AI: usize = 400;
const DONE_MAX: usize = 32;

struct Saving {
    token: u64,
    reply: Reply,
    ok: Value,
}

#[derive(Default)]
pub struct State {
    saving: Vec<Saving>,
    done: Vec<(u64, Result<Value, String>)>,
}

impl State {
    fn answer(&mut self, token: u64, r: Result<Value, String>) {
        self.done.push((token, r));
        if self.done.len() > DONE_MAX {
            drop(self.done.remove(0));
        }
    }

    pub fn take(&mut self, token: u64) -> Option<Result<Value, String>> {
        let i = self.done.iter().position(|d| d.0 == token)?;
        Some(self.done.remove(i).1)
    }
}

fn fields() -> Value {
    let kinds: Vec<&str> = KINDS.iter().map(|k| k.id).collect();
    json!({
        "kind": {"type": "string", "enum": kinds},
        "path": {"type": "string", "description": "Only for kind file: the full path of the picture or video on the computer Corvus GCS runs on"},
        "name": {"type": "string", "description": "At most 60 characters"},
        "note": {"type": "string", "description": "At most 300 characters"},
        "color": {"type": "string", "description": "#RRGGBB"},
        "lat": {"type": "number"},
        "lon": {"type": "number"},
        "lat2": {"type": "number", "description": "A gate's second post"},
        "lon2": {"type": "number", "description": "A gate's second post"},
        "radius": {"type": "number", "description": "Metres; not for a gate. 0 for none"},
        "height": {"type": "number", "description": "Metres; a gate's opening or a tree"},
        "base": {"type": "number", "description": "Metres above home where a gate's opening begins"},
        "angle": {"type": "number", "description": "Degrees the area covers, 360 for a full circle; not for a gate or a tree"},
        "bearing": {"type": "number", "description": "Degrees from north the area faces"},
        "alert": {"type": "boolean", "description": "Tell the operator when the aircraft enters the area"},
    })
}

/// The tools the two switches allow.
pub fn tools(read: bool, edit: bool) -> Vec<McpTool> {
    if !read && !edit {
        return Vec::new();
    }
    let tool = |name: &str, title: &str, description: &str, schema: Value, read_only: bool| McpTool {
        name: name.into(),
        title: title.into(),
        description: description.into(),
        input_schema: schema,
        confirm: false,
        read_only,
    };
    let id = json!({"type": "object", "properties": {"id": {"type": "string"}}, "required": ["id"]});
    let mut out = vec![
        tool(
            "list_entities",
            "List the map entities",
            "The gates, trees and symbols on the Home map: where they are, their size, and what the aircraft did to each this session (passes and misses of a gate, hits of a tree, entries into an area).",
            json!({"type": "object", "properties": {}}),
            true,
        ),
        tool(
            "show_entity",
            "Show a map entity",
            "Frame one entity on the operator's Home map, or all of them without an id.",
            json!({"type": "object", "properties": {"id": {"type": "string"}}}),
            false,
        ),
    ];
    if edit {
        let props = fields();
        out.push(tool(
            "add_entity",
            "Place a map entity",
            "Place a gate (two posts), a tree, a symbol with an optional area, or a file marker on the Home map. Kind file marks where a picture or a video was recorded: take lat and lon from its metadata, give its path, and put the time into note. The operator opens it with a double click. All of it is only drawn: nothing is sent to the aircraft. Returns the entity as it was placed.",
            json!({"type": "object", "properties": props.clone(), "required": ["kind", "lat", "lon"]}),
            false,
        ));
        let mut props = props;
        if let Some(o) = props.as_object_mut() {
            o.remove("kind");
            o.insert("id".into(), json!({"type": "string"}));
        }
        out.push(tool(
            "update_entity",
            "Change a map entity",
            "Change an entity you added. What you leave out stays as it was.",
            json!({"type": "object", "properties": props, "required": ["id"]}),
            false,
        ));
        out.push(tool(
            "remove_entity",
            "Remove a map entity",
            "Remove an entity you added.",
            id,
            false,
        ));
    }
    out
}

fn text<'a>(args: &'a Value, key: &str) -> Result<Option<&'a str>, String> {
    match args.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(s)) => Ok(Some(s.trim())),
        Some(_) => Err(format!("{key} must be a string")),
    }
}

fn id_arg(args: &Value) -> Result<&str, String> {
    text(args, "id")?
        .filter(|s| !s.is_empty())
        .ok_or_else(|| "id is required".into())
}

/// What an AI client reads of one entity.
pub fn describe(e: &Entity, r: Option<&Results>) -> Value {
    let mut v = e.to_json();
    if let Some(m) = v.as_object_mut() {
        for (from, to) in [("lng", "lon"), ("lng2", "lon2")] {
            if let Some(x) = m.remove(from) {
                m.insert(to.into(), x);
            }
        }
        m.remove("by_ai");
        // A path a client gave is its own to read again. Where the operator's files
        // lie is not: of those it learns the name.
        if !e.by_ai && m.remove("path").is_some() && !e.path.is_empty() {
            m.insert("file".into(), json!(e.file_name()));
        }
        m.insert("added_by_ai".into(), json!(e.by_ai));
        if let Some(r) = r {
            let did = if e.is_gate() {
                json!({"passes": r.passes, "misses": r.misses})
            } else if e.is_tree() {
                json!({"hits": r.hits})
            } else {
                json!({"entries": r.entries, "inside_now": r.inside})
            };
            m.insert("this_session".into(), did);
        }
    }
    v
}

/// An entity from an AI client's arguments, merged over `base`.
pub fn entity_from(args: &Value, base: Option<&Entity>, fresh_name: impl Fn(&str) -> String) -> Result<Entity, String> {
    let mut raw: Map<String, Value> = base
        .map(Entity::to_json)
        .and_then(|v| v.as_object().cloned())
        .unwrap_or_default();
    match base {
        Some(_) if args.get("kind").is_some_and(|k| !k.is_null()) => {
            return Err("The kind of an entity cannot be changed. Remove it and place another.".into())
        }
        Some(_) => {}
        None => {
            let kind = text(args, "kind")?.unwrap_or("");
            if !KINDS.iter().any(|k| k.id == kind) {
                let all: Vec<&str> = KINDS.iter().map(|k| k.id).collect();
                return Err(format!("kind must be one of {}", all.join(", ")));
            }
            raw.insert("kind".into(), json!(kind));
            raw.insert("name".into(), json!(fresh_name(kind)));
        }
    }
    for key in ["name", "note", "color"] {
        if let Some(s) = text(args, key)? {
            if key == "color" && corvus_api::plugin::shapes::hex(s).is_none() {
                return Err("color must be #RRGGBB".into());
            }
            if key != "name" || !s.is_empty() {
                raw.insert(key.into(), json!(s));
            }
        }
    }
    for (key, to) in [
        ("lat", "lat"),
        ("lon", "lng"),
        ("lat2", "lat2"),
        ("lon2", "lng2"),
        ("radius", "radius"),
        ("height", "height"),
        ("base", "base"),
        ("angle", "angle"),
        ("bearing", "bearing"),
    ] {
        match args.get(key) {
            None | Some(Value::Null) => {}
            Some(v) => {
                let n = v.as_f64().filter(|n| n.is_finite());
                raw.insert(to.into(), json!(n.ok_or_else(|| format!("{key} must be a number"))?));
            }
        }
    }
    if let Some(path) = text(args, "path")? {
        if raw.get("kind") != Some(&json!(FILE_KIND)) {
            return Err("path is only for kind file".into());
        }
        raw.insert("path".into(), json!(media_path(path)?));
    }
    match args.get("alert") {
        None | Some(Value::Null) => {}
        Some(Value::Bool(b)) => {
            raw.insert("alert".into(), json!(b));
        }
        Some(_) => return Err("alert must be true or false".into()),
    }
    let mut e = normalize_entity(&Value::Object(raw))
        .ok_or("That is no place on the map. lat and lon are needed, and lat2 and lon2 too for a gate.")?;
    e.by_ai = true;
    Ok(e)
}

impl MapEntities {
    fn mcp_allowed(h: &Host) -> Vec<McpTool> {
        let on = |k: &str| h.options().get(k) == Some(&Value::Bool(true));
        tools(on(MCP_READ), on(MCP_EDIT))
    }

    pub(crate) fn mcp_tools_for(h: &Host) -> Vec<McpTool> {
        Self::mcp_allowed(h)
    }

    /// Starts one tool call; the answer comes from `mcp.take`.
    pub(crate) fn mcp_start(&mut self, name: &str, args: &Value, token: u64, h: &mut Host) {
        match self.mcp_now(name, args, h) {
            Ok((v, None)) => self.mcp.answer(token, Ok(v)),
            Ok((ok, Some(reply))) => self.mcp.saving.push(Saving { token, reply, ok }),
            Err(e) => self.mcp.answer(token, Err(e)),
        }
    }

    /// Moves the calls that wait on a save.
    pub(crate) fn mcp_run(&mut self) {
        for s in std::mem::take(&mut self.mcp.saving) {
            match s.reply.take() {
                None => self.mcp.saving.push(s),
                Some(Ok(_)) => self.mcp.answer(s.token, Ok(s.ok)),
                Some(Err(e)) if e.is_empty() => {
                    self.mcp.answer(s.token, Err("The entities could not be saved.".into()))
                }
                Some(Err(e)) => self.mcp.answer(s.token, Err(e)),
            }
        }
    }

    fn own(&self, id: &str) -> Result<Entity, String> {
        let e = self
            .find(id)
            .ok_or_else(|| format!("No entity {id}. list_entities names them."))?;
        if !e.by_ai {
            return Err("That entity is the operator's. You may change only the ones you added.".into());
        }
        Ok(e.clone())
    }

    fn mcp_now(&mut self, name: &str, args: &Value, h: &mut Host) -> Result<(Value, Option<Reply>), String> {
        if !Self::mcp_allowed(h).iter().any(|t| t.name == name) {
            return Err(format!("The plugin offers no tool {name} right now."));
        }
        match name {
            "list_entities" => {
                let list: Vec<Value> = self
                    .entities
                    .iter()
                    .map(|e| describe(e, self.results.get(&e.id)))
                    .collect();
                Ok((json!({"entities": list, "max": MAX_ENTITIES}), None))
            }
            "show_entity" => {
                let points = match text(args, "id")?.filter(|s| !s.is_empty()) {
                    Some(id) => points_of(
                        self.find(id)
                            .ok_or_else(|| format!("No entity {id}. list_entities names them."))?,
                    ),
                    None => self.all_points(),
                };
                if points.is_empty() {
                    return Err("Nothing is placed on the map.".into());
                }
                h.fit(points);
                Ok((json!({"ok": true}), None))
            }
            "add_entity" => {
                if self.entities.len() >= MAX_ENTITIES {
                    return Err(format!("The map is full ({MAX_ENTITIES} entities)."));
                }
                if self.entities.iter().filter(|e| e.by_ai).count() >= MAX_BY_AI {
                    return Err(format!(
                        "You may place {MAX_BY_AI} entities. Remove some you added first."
                    ));
                }
                let e = entity_from(args, None, |kind| {
                    super::model::kind_of(kind).map_or_else(String::new, |k| super::model::next_name(&self.entities, k))
                })?;
                let out = describe(&e, None);
                // Not selected: the operator may be working on another one.
                self.draw_entity(&e, h);
                self.entities.push(e);
                Ok((out, Some(self.save_reply(h))))
            }
            "update_entity" => {
                let before = self.own(id_arg(args)?)?;
                let after = entity_from(args, Some(&before), |_| String::new())?;
                let out = describe(&after, None);
                if let Some(e) = self.entities.iter_mut().find(|e| e.id == before.id) {
                    *e = after.clone();
                }
                // What the aircraft did, it did to the entity as it was.
                self.results.remove(&before.id);
                self.draw_entity(&after, h);
                Ok((out, Some(self.save_reply(h))))
            }
            "remove_entity" => {
                let e = self.own(id_arg(args)?)?;
                self.remove(&e.id, h);
                Ok((json!({"removed": e.id}), Some(self.save_reply(h))))
            }
            other => Err(format!("The plugin offers no tool {other} right now.")),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn names(read: bool, edit: bool) -> Vec<String> {
        tools(read, edit).into_iter().map(|t| t.name).collect()
    }

    #[test]
    fn a_client_places_a_file_with_its_path_and_learns_no_path_of_the_operators() {
        let make = |args: Value| entity_from(&args, None, |_| "File 1".into());
        let args = json!({"kind": "file", "lat": 48, "lon": 11, "path": "~/DCIM/DJI_0001.JPG", "note": "14:02:11"});
        let placed = make(args).unwrap();
        assert_eq!((placed.path.as_str(), placed.by_ai), ("~/DCIM/DJI_0001.JPG", true));
        assert_eq!(describe(&placed, None)["path"], "~/DCIM/DJI_0001.JPG");
        for good in ["/home/p/a b.mp4", "C:\\photos\\a.jpg", "D:/a.MOV"] {
            assert!(
                make(json!({"kind": "file", "lat": 48, "lon": 11, "path": good})).is_ok(),
                "{good}"
            );
        }
        for bad in [
            "a.jpg",
            "../a.jpg",
            "//server/share/a.jpg",
            "\\\\server\\a.jpg",
            "http://x/a.jpg",
            "/tmp/run.sh",
            "/etc/passwd",
        ] {
            let why = make(json!({"kind": "file", "lat": 48, "lon": 11, "path": bad})).unwrap_err();
            assert!(why.starts_with("path must"), "{bad}: {why}");
        }
        let why = make(json!({"kind": "tree", "lat": 48, "lon": 11, "path": "/a.jpg"})).unwrap_err();
        assert_eq!(why, "path is only for kind file");
        let moved = entity_from(&json!({"path": "/other/b.png"}), Some(&placed), |_| String::new()).unwrap();
        assert_eq!(moved.path, "/other/b.png");

        let mut own = placed;
        own.by_ai = false;
        own.path = "/home/pilot/secret/flight.mp4".into();
        let told = describe(&own, None).to_string();
        assert!(
            told.contains("\"file\":\"flight.mp4\"") && !told.contains("secret"),
            "{told}"
        );
    }

    #[test]
    fn the_switches_decide_which_tools_there_are() {
        assert!(names(false, false).is_empty());
        assert_eq!(names(true, false), ["list_entities", "show_entity"]);
        assert_eq!(names(false, true).len(), 5, "placing needs the list too");
        for t in tools(true, true) {
            assert_eq!(t.input_schema["type"], "object", "{}", t.name);
            assert!(t.input_schema.to_string().len() < 4000, "{}", t.name);
        }
    }

    fn add(args: Value) -> Result<Entity, String> {
        entity_from(&args, None, |_| "Tree 1".into())
    }

    #[test]
    fn an_entity_is_made_from_the_arguments_and_marked() {
        let e = add(json!({"kind": "tree", "lat": 48.0, "lon": 11.0, "height": 20})).unwrap();
        assert!(e.by_ai);
        assert_eq!((e.name.as_str(), e.lat, e.lng, e.height), ("Tree 1", 48.0, 11.0, 20.0));
        let back = normalize_entity(&e.to_json()).unwrap();
        assert_eq!(back, e, "the mark is saved");
        let d = describe(&e, None);
        assert_eq!((&d["lon"], &d["added_by_ai"]), (&json!(11.0), &json!(true)));
        assert!(d.get("lng").is_none());
        let g = add(json!({"kind": "gate", "lat": 48, "lon": 11, "lat2": 48.0001, "lon2": 11})).unwrap();
        assert!(g.is_gate());
    }

    #[test]
    fn what_is_no_entity_is_refused_with_a_reason() {
        for (args, why) in [
            (json!({"lat": 1, "lon": 1}), "kind must be"),
            (json!({"kind": "ufo", "lat": 1, "lon": 1}), "kind must be"),
            (json!({"kind": 3, "lat": 1, "lon": 1}), "must be a string"),
            (json!({"kind": "tree", "lon": 1}), "no place"),
            (json!({"kind": "tree", "lat": 91, "lon": 1}), "no place"),
            (json!({"kind": "gate", "lat": 1, "lon": 1}), "no place"),
            (json!({"kind": "tree", "lat": "x", "lon": 1}), "lat must be a number"),
            (
                json!({"kind": "tree", "lat": 1, "lon": 1, "color": "red"}),
                "color must be",
            ),
            (
                json!({"kind": "tree", "lat": 1, "lon": 1, "alert": "yes"}),
                "alert must be",
            ),
        ] {
            let e = add(args.clone()).unwrap_err();
            assert!(e.contains(why), "{args}: {e}");
        }
        let big = add(json!({"kind": "radar", "lat": 1, "lon": 1, "radius": 1e300})).unwrap();
        assert_eq!(big.radius, 100_000.0);
    }

    #[test]
    fn a_change_keeps_what_it_does_not_name() {
        let e = add(json!({"kind": "radar", "lat": 1, "lon": 2, "radius": 70, "note": "n"})).unwrap();
        let c = entity_from(&json!({"lat": 3, "name": "North"}), Some(&e), |_| String::new()).unwrap();
        assert_eq!((c.id.as_str(), c.lat, c.lng), (e.id.as_str(), 3.0, 2.0));
        assert_eq!(
            (c.name.as_str(), c.radius, c.note.as_str(), c.by_ai),
            ("North", 70.0, "n", true)
        );
        assert!(entity_from(&json!({"kind": "tree"}), Some(&e), |_| String::new()).is_err());
    }

    #[test]
    fn an_operators_entity_is_not_the_clients() {
        let mut p = MapEntities::default();
        let mut e = add(json!({"kind": "tree", "lat": 1, "lon": 2})).unwrap();
        p.entities.push(e.clone());
        assert!(p.own(&e.id).is_ok());
        e.by_ai = false;
        p.entities[0] = e.clone();
        assert!(p.own(&e.id).unwrap_err().contains("operator's"));
        assert!(p.own("nope").unwrap_err().contains("No entity"));
    }

    #[test]
    fn answers_are_taken_once_and_do_not_pile_up() {
        let mut s = State::default();
        for i in 0..100 {
            s.answer(i, Ok(json!(i)));
        }
        assert_eq!(s.done.len(), DONE_MAX);
        assert_eq!(s.take(99), Some(Ok(json!(99))));
        assert_eq!(s.take(99), None);
    }
}
