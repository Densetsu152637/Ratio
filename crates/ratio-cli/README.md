# ratio-cli

The `ratio` command (stage C, "checker-complete"): it checks `.ratio` sources and packages on
disk with the whole front end of the [`ratio` library](../ratio/README.md): the parser
(`P` codes), modules and names (`N`), and the type checker with schemas and taint (`C`). It
runs nothing. The crate is a binary only (`publish = false`) and depends on nothing outside
this repository; its arguments are parsed by hand.

## Install and run

```sh
cargo install --path crates/ratio-cli --locked     # from a checkout; installs `ratio`
ratio check harness.ratio
cargo run -p ratio-cli -- check harness.ratio      # without installing
```

## Commands

```text
ratio check [--json] <path>...  Check .ratio packages: parse, names, types and taint
ratio help [<command>]          Show this help, or a command's
ratio --version                 Show the ratio-lang version, spec draft and stage
```

`ratio --version` prints `ratio <ratio-lang version> (spec draft <SPEC_DRAFT>, stage <STAGE>)`.
`ratio lint` (stage L) and `ratio run` (a later stage) are reserved: they say so and exit 2.

### `ratio check`

Each path is checked as its own `user:` package:

- a `.ratio` **file** is the package's entry, and the folder it is in is its source root;
- a **folder** is a package whose entry is `main.ratio`, else the only `.ratio` file directly
  in it with a `meta` block (with several, the command asks for one to be named).

Imports by path stay inside the source root (chapter 03). Packages named on the same command
line may import one another as `user:<name>`, where the name is the folder's (for a folder) or
the file's without `.ratio` (for a file), when that is a valid package name, else `main`; a
name already taken gets a number (`main_2`). The `std:` packages arrive with stage 23 and are
not shipped yet, so a `std:` import is reported as not found, like any missing package (an `N`
diagnostic, not an I/O error).

Output, one diagnostic after another, sorted by file and position:

```text
conformance/types/err-state-undeclared.ratio:7:11: error C0001: `state.get` needs a `state` declaration in the package, and it has none
7 |   let s = state.get()
  |           ^^^^^^^^^^^
```

The first line is `file:line:col: severity CODE: message`; the source line and the caret under
the span are drawn as the syntax crate's `Diagnostic::render` draws them. Everything printed
for a person (file names, messages, source lines, and the paths and reasons on standard error)
comes from the package, so a character that could control the terminal or reorder text is
written as an escape instead: C0 controls other than tab, DEL, C1 controls (U+0080 to U+009F),
the bidirectional embeddings, overrides, isolates and marks (U+202A to U+202E, U+2066 to
U+2069, U+200E, U+200F, U+061C), the invisible joiners, spaces and operators (U+200B to
U+200D, U+2060 to U+2064, U+FEFF, U+180E), the soft hyphen, the variation selectors (U+FE00 to
U+FE0F, U+E0100 to U+E01EF),
the line and paragraph separators (U+2028, U+2029), the interlinear annotation marks (U+FFF9
to U+FFFB) and the tag characters (U+E0000 to U+E007F) appear as `\u{1b}`, `\u{202e}`, …, and
in messages and source lines a `\` is doubled, so a `\u{1b}` written in the source cannot pass
for one the tool escaped. The caret is placed under the escaped line. An argument that is not
valid UTF-8 is a usage error (exit 2), shown lossily and escaped. An OSC 52
clipboard write, a screen clear, a hyperlink or a Trojan Source reordering in a package
therefore never reaches the terminal. JSON escapes control characters itself. A summary
(`1 package: 1 error, 0 warnings`) goes to standard error. With `--json`, each diagnostic is one
JSON object on its own line, and nothing else is printed to standard output:

```json
{"file":"a.ratio","line":7,"column":11,"end_line":7,"end_column":22,"severity":"error","code":"C0001","message":"…"}
```

`line`, `column`, `end_line` and `end_column` are absent for a diagnostic without a position
(an entry that could not be read). `--` makes every later argument a path.

**Exit status**: 0 when every package is clean (warnings allowed), 1 when any has an error,
2 for a usage error or a path that names no package (missing, not a `.ratio` file or a
folder, a link, or a folder with no entry), and for an internal error of the checker's table.
A usage or I/O error wins over errors in the other paths.

## Reading files

Packages are read through `FsRegistry` ([`src/fsreg.rs`](src/fsreg.rs)), the file-system
implementation of `ratio-names`' `Registry` trait, which keeps the duties the trait's
documentation gives a host as far as the standard library allows, without `unsafe` or platform
crates:

- each path is walked from the package root one component at a time; a component that is a
  symbolic link, junction or other reparse point, seen without following it, is refused
  (`RegistryError::Link`), and so is a package root that is itself a link;
- every component must appear with exactly that spelling in its folder's listing, so a name
  that matches only ignoring case is not found, even on a case-insensitive file system;
- the size is read from the opened handle, and a file over the loader's limit (256 KiB) is
  refused without being read; at most one byte more than the limit is ever read;
- on Windows the file is opened without following a reparse point and its attributes are
  checked on the handle; on Unix the opened handle must be the device and inode the walk saw,
  with a single link (a hard link is refused).

**Residuals, accepted for a local developer tool.** The standard library has no `openat`, so a
folder swapped for a link between the walk and the open is not detected; on Windows it has no
stable way to count a file's links, so hard links are not refused there; and the folders
*above* the package root are the person's choice on the command line and are not checked (a
temporary folder under a linked `/tmp` still works), though the root itself must not be a link.
The command checks files its user chose on their own machine, and it runs nothing, so these
were reviewed (stage C, security review round 1) and accepted for it. They would not be for a
host serving model-written or third-party packages: Imperium's registry (chapter 10, stage 18)
keeps the trait's duties with the platform's own calls.

## Tests

`tests/cli.rs` runs the built binary: `--version` and `help`; usage errors and missing paths
(exit 2); a clean file (exit 0, nothing on standard output) and warnings only (exit 0); the
human form with its caret and the `--json` form byte for byte; every file of
`conformance/types/` and `conformance/taint/`, whose headers must match what the command
reports; folder entries (`main.ratio`, the one `meta` file, several); a path import that leaves
the root; packages importing each other and a `std:` import; links and hard links (Unix);
junctions and file links (Windows, made with `mklink /J`; skipped with a note where that is not
possible); a file over the size limit; escape sequences and bidi controls in a source line, an
import path, a path argument and (Unix) a file name, which never reach the terminal; and the eight example packages of chapter 09, which must check clean but
for the built-ins stages 8 and 9 add and the listed errata and lint warnings.
`src/render.rs` has unit tests of the two output forms and the JSON escaping.
