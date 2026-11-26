# ratio-names

Ratio's modules and names: stage 3 of the [implementation plan](../../spec/10-implementation.md).
Hosts do not depend on this crate; they use the [`ratio` facade](../ratio/README.md), which
re-exports it as `ratio::names`.

## What it does

A host keeps packages in five **sources**, in decreasing order of trust: `std`, `user`,
`plugin:<plugin>`, `project`, `authored:<area>` (chapter 07, "Who writes Ratio"; chapter 08,
"Where harnesses come from"). It serves them through the `Registry` trait, and the `Loader`
loads a package by its source-qualified name:

```rust
pub trait Registry {
    fn find(&self, source: &Source, name: &PackageName) -> Result<Option<RelPath>, RegistryError>;
    fn read(&self, package: &PackageId, path: &RelPath, max_bytes: usize)
        -> Result<Vec<u8>, RegistryError>;
}

let loaded = Loader::new(&host_registry, &builtin).load(&QualifiedName::parse("user:critics.strict")?);
```

- `find` returns a package's entry file, relative to the package's **source root** (the folder
  the entry was found in); `read` returns a file's bytes. Loading is checking and nothing runs,
  so the trait is synchronous. Imperium's daemon registry implements it (stage 18).
- `std:` names are looked up only in the built-in registry the loader is given (the facade embeds
  `std/` from stage 23), never in the host's, so a host cannot supply a `std` package.
- The trait's documentation lists what a host must guarantee, since the loader cannot see the
  file system: open each component from an open handle on the package root without following
  links (package root and source folders included), check attributes on the opened handle,
  refuse hard links (`nlink > 1`), match names case-exactly, check the size on the opened
  handle and refuse above `max_bytes` without reading (the loader re-checks and fails closed). `RegistryError` carries no text, so a host's error never
  reaches a diagnostic. Pinning is the host's: the daemon hashes `LoadedModule::source`, and its
  `find` chooses which authored version is current.
- `MemoryRegistry` keeps packages in memory: the test registry, and the store for built-ins.

The loader reads and parses each file once, follows imports, and returns every module with its
tree, its syntax diagnostics, the module each import loaded and its top-level `Scope`, plus the
loading and name diagnostics (the `N` codes below).

### Imports by path

`import "./lib/critics.ratio" as critics` is relative to the importing file and stays inside the
package's source root. The path is normalised before any registry sees it (`RelPath`): `..`
that leaves the root (`N0002`) is refused, and so (`N0001`) are absolute paths, empty
components, components ending in a dot, Windows device names (`con.ratio`), anything but a
`.ratio` file, and any character but ASCII letters, digits, `_`, `-`, `.` and `/` (so no `\`,
`:`, `~` short names, spaces, or Unicode look-alikes and normalisation forms). A registry must not follow links: it reports a symbolic
link, junction or reparse point as `RegistryError::Link`, which the loader refuses (`N0003`). Two
paths of one package that differ only in letter case are refused (`N0015`), so a package means the
same on every file system.

### Imports by name, and trust

- A qualified name names one source: `std:debate`, `user:critics.strict`, `plugin:acme/review`,
  `project:kit`, `authored:session/vote`, `authored:env/vote`. In an authored package's import,
  `authored:vote` looks only in the importer's own area; for the host's entry it looks in the
  session's area, then the environment's. Package and plugin names are words of ASCII letters,
  digits and `_`, not keywords, joined by `.` (a plugin's name is one such word), at most 128 bytes,
  never a device name (`PackageName`, `PluginName`, `N0006`). `@"^1.2"` pins a range (`^`, `~` or `=`), checked
  against the `version` of the package's `meta` (`N0008`).
- A **bare** name (`import debate`) looks in `std`, then `user`. A lower source never shadows a
  higher one, and a plugin's, project's or model-authored package is reached only by its qualified
  name: a bare name never picks one up unasked (`N0005`).
- **Trust follows source** (`Source::may_import`): every package may import `std`; a `std`
  package nothing else; a user package user, plugin and project packages; a plugin package only
  its own plugin's; a project package only its project's; an authored package only its own
  area's (`N0007`). The entry package is the host's choice and is not checked.

### Names

Each module's `Scope` binds its items (`type`, `enum`, `const`, `fn`, `harness`, `prompt`) and
what its imports bind: the alias; without an alias or a list, the last word of the name
(`std:debate` binds `debate`) or the file name (`critics`); and each public item of an import
list (`{ open, close }`). `Scope::resolve` looks there first, then in the `prelude` (the standard
modules, the `data` submodules by their short names, `log`, `trace`, and `control`'s functions).
The checker (stage 4) puts local scopes in front and resolves what needs types (contextual
variants, tool names, methods).

### Limits

Source text is hostile. Files are read at most once, imports are followed with an explicit stack,
and `Limits` bound the work: 256 KiB per file, 2 MiB per package (64 KiB for an authored package),
1024 files, 64 packages and 4096 registry calls per load, and at most 64 modules in one import
chain, the entry counted (`N0010`, `N0011`). Every registry answer, failures included, is
remembered, so a name or path is asked about once. Errors never panic.

## Diagnostic codes

| Code | Diagnostic |
| --- | --- |
| `N0001` | An import path that is not a plain relative path to a `.ratio` file |
| `N0002` | An import path whose `..` leaves the package's source root |
| `N0003` | An import path that is, or passes through, a link |
| `N0004` | No file at an import path |
| `N0005` | No package by that name (a bare name looks in `std` and `user` only) |
| `N0006` | A malformed source-qualified name or version range |
| `N0007` | An import the trust rules refuse |
| `N0008` | A package whose `meta` version is missing or outside the import's range |
| `N0009` | An import cycle |
| `N0010` | A file or package over its size limit |
| `N0011` | More files or packages, or deeper imports, than the loader's limits |
| `N0012` | A second definition of a name in a module |
| `N0013` | An import list naming something the module does not define |
| `N0014` | An import list naming an item that is not `pub` |
| `N0015` | Two paths of a package that differ only in letter case |
| `N0016` | A file the registry could not read, or that is not UTF-8 |
| `N0017` | An imported module with syntax errors |
| `N0018` | An import that binds no name (the file name is not an identifier); add `as` |
| `N0019` | `pub` on an import (imports are not re-exported) |
| `N0020` | Warning: a name that shadows a standard module |
| `N0021` | Warning: a name that shadows `trust`, `as_task` or `declassify` |

## Tests

- `tests/conformance.rs` runs `conformance/names/`: each folder is a world of source roots laid
  out as `<source>/<package>/…` (`plugin/<plugin>/…`, `authored/<area>/…`), with one entry file
  whose header says `resolve-ok`, or lists `resolve-error CODE at LINE:COL [in FILE]` and
  `resolve-warning …` lines. Every code has a world, except those only a host or a limit can
  trigger (`N0003`, `N0010`, `N0011`, `N0016`).
- `tests/loader.rs` covers those, which package a name resolved to, the trust rules as a table,
  the scope's order (items and imports, then the prelude), hostile graphs (a 200-file chain,
  500 imports of one file read once, 1200 failing imports costing six registry calls, the call
  cap, an over-long answer), names outside ASCII, and the authored area lookup.
