# Map Entities for Corvus GCS

A plugin for [Corvus GCS](https://github.com/M-BSquared/CorvusGCS), the
offline ground control station for PX4 and ArduPilot.

Gates, trees, people and devices with a range, placed exactly on the Home map
and checked against the aircraft's track: through a gate, over it or under it,
and whether it entered an area. A file mark holds the path of a picture or video
recorded at its place and shows it on a double click; an AI client allowed to
place things can set these from a picture's metadata.

Install it in Corvus GCS under Settings, Plugins. The full description is in
the [Corvus guide](https://m-bsquared.github.io/CorvusGCS/guide/plugins.html#map-entities).

## What it is

A Rust crate (`corvus_plugin_map_entities`) built on `corvus_api`, the plugin API of Corvus GCS, and
built into a WebAssembly module that every system runs, published from this repository.

- `src/model.rs`: the rules, held to what the web plugin answered.
- `src/mcp.rs`: the tools for an AI client, off until the gear allows them.
- `src/lib.rs`: the plugin and the map.
- `src/view.rs`: what is drawn.
- `lang/`: German, French and the dialect file.

## Change it

Corvus GCS carries this repository as a submodule at `plugins/map-entities`, and the
crate is a member of its workspace:

```bash
git clone --recurse-submodules https://github.com/M-BSquared/CorvusGCS.git
cd CorvusGCS
./run.sh
```

Settings, Plugins then offers the copy built there. A dependency must be one
that `Cargo.lock` of Corvus GCS already holds.

## Publish it

Commit, then:

```bash
./release.sh
```

It runs the tests and pushes `main`. The workflow builds the plugin once, with the oldest Corvus
GCS that `plugin.json` names (`min_corvus`) or the first published one after it, lets that program
load it, and attaches it to the release `plugin` of this repository. Every later Corvus GCS takes
it as it is. Raise `min_corvus` when the plugin starts to need something newer: older stations
then go on getting the last version that fits them. `plugin.json` also names the permissions the
plugin uses, which the operator sees before installing it. `--watch` waits for the result, `--help`
lists the rest.

## Tests

From the Corvus checkout:

```bash
cargo test -p corvus_plugin_map_entities
```

## Licence

Sustainable Use License, see [LICENSE.md](LICENSE.md). Copyright (c) 2026
Maximilian Böck.
