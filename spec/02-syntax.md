# 02 Syntax

## Decision R4: small, expression-oriented, brace-delimited

Ratio is **expression-oriented**: `if`, `match`, `try`, `par`, `race`, `with` and blocks are
expressions with values, so a harness reads as a computation that produces its result
(`let verdict = if passed { Pass } else { critique(work) }`). Statements exist only for bindings,
assignment and `defer`. The surface borrows from Rust, Swift and Kotlin because the people who
write harnesses will mostly know one of them; it drops lifetimes, ownership, traits, macros,
classes and inheritance.

Rejected: a statement-based, Python-like surface. Significant indentation is fragile in
generated diffs and in prompts embedded as multi-line strings, and statements would make the
common "compute a value under conditions" pattern longer. Rejected: a Lisp. Readable to few of
the intended authors.

## Lexical structure

**Source.** UTF-8 text. A byte-order mark is ignored. Line ends are `\n` or `\r\n`.

**Comments.** `//` to the end of the line. `///` is a documentation comment attached to the next
item or record field; `harnesses.list` shows it and schema derivation uses it as a field's
`description`.

**Identifiers.** `[A-Za-z_][A-Za-z0-9_]*`. By convention types are `UpperCamel`; variants of
option-like enums (unit variants only, such as `Effort.high`) are `snake_case`, because they are
also their JSON spelling; other variants are `UpperCamel`; everything else is `snake_case`. The
checker warns on other spellings. `_` alone is the wildcard pattern.

**Keywords** (reserved everywhere except after `.` and `::`, see below): `and as break catch const
continue defer do else enum false fn for harness if import in is let loop match none not or
prompt pub raise repeat return test true try type var while with`.

**Contextual keywords.** These are ordinary identifiers except in one position each:

| Word | Keyword only | Elsewhere |
| --- | --- | --- |
| `meta` | at the start of the file's `meta { … }` header | the `meta` module (`meta.frame()`) |
| `state` | at the start of an item (`state: T = …`) | the `state` module (`state.get()`) |
| `par`, `race` | when the next token is `{` | the functions `par([…])`, `race([…])` |
| `timeout` | after `with` | an ordinary name |
| `system instruct task data example format steps context note include image` | at the start of a prompt section | ordinary names (`context.main()` inside a prompt section is the `context` section followed by an expression) |

**Members.** After `.`, `?.` and `::`, any keyword is an identifier, so `model.loop`,
`meta.harness` and `e.kind is Denied` need no escaping.

**Literals.**

| Literal | Examples | Type |
| --- | --- | --- |
| Integer | `0`, `42`, `1_000`, `0x1F` | `Int` (64-bit signed; overflow is an `Invalid` error, never wraps) |
| Quantity | `20k`, `1.5M` | `Int`, scaled by 1,000 or 1,000,000; the result must be whole |
| Float | `0.5`, `1e-3`, `2.0` | `Float` (IEEE 754 binary64) |
| Duration | `500ms`, `30s`, `5m`, `2h` | `Duration` |
| Money | `$0.50`, `$12` | `Money` (US dollars, exact to 1e-6) |
| Boolean | `true`, `false` | `Bool` |
| None | `none` | `T?` for any `T` |
| String | `"Hello, {name}!"` | `Str`, or `Untrusted<Str>` if an interpolated part is untrusted |
| Raw string | `r"C:\path\{not interpolated}"` | `Str` |
| Long string | `"""` … `"""` | `Str`; common indentation and the first and last blank lines are removed |
| Raw long string | `r"""` … `"""` | `Str`, no interpolation, same dedent |

### Strings and interpolation

The lexer has a mode stack. In **code mode** it reads tokens; `"`, `"""`, `r"` and `r"""` push a
**string mode**. In string mode:

```ebnf
string_body     = { string_char | escape | interpolation } ;
string_char     = ? any character except "\", "{", "}" and the closing quote;
                    a line break only in long strings ? ;
escape          = "\" , ( "n" | "r" | "t" | "\" | '"' | "{" | "}" | "u{" , hex , { hex } , "}" ) ;
interpolation   = "{" , code_tokens , [ ":" , format_spec ] , "}" ;
format_spec     = { ? any character except "{", "}" and line breaks ? } ;
```

- `{` pushes code mode with a brace depth of 0. In it, `(`, `[` and `{` raise the depth and
  `)`, `]` and `}` lower it; a `:` at depth 0 starts the format spec (so a record literal or a
  `match` inside an interpolation must be parenthesised), and a `}` at depth 0 ends the
  interpolation and pops back to string mode.
- Interpolations nest: a string inside an interpolation pushes another string mode, so
  `"{if ok { "yes" } else { "no" }}"` is valid. The depth of nesting is limited (chapter 07).
- In a short string an interpolation may not contain a line break; in a long string it may.
- A lone `}` in string mode is an error; write `\}`.
- Raw strings have no escapes and no interpolation; `{` and `}` are characters.
- `{expr}` inserts `text.show(expr)`; `{expr:fmt}` applies a format spec: `.2` (two decimals),
  `>6` and `<6` (right and left alignment in a width), `json` (compact JSON).

**Statement ends.** The grammar uses `;` as a terminator. The parser's token reader inserts one at
a line end when **all** of these hold:

1. the innermost open delimiter is a **block** or a **prompt body** (never `( )`, `[ ]`, a record
   literal or record type, an enum body, a `match` or `catch` arm list, or a declaration head from
   `fn`, `harness` or `prompt` up to the `{` of its body);
2. the line's last token is an identifier, a literal, `)`, `]`, `}`, `?`, or one of `break`,
   `continue`, `return`, `none`, `true`, `false`;
3. the next non-blank line does not start with `.`, `?.`, `|>`, `??`, `and`, `or`, `else` or
   `catch`.

So pipelines and `else` on the next line continue an expression; inside a `match`, `catch`, enum
or record the line breaks are only whitespace and the entries are separated by commas; a
harness's `with` configuration and body may start on lines of their own. A `with` at the start of
a line in a block always begins a new `with` expression statement; a call override
(`f(x) with { … }`) must start on the call's line.

## Operators and precedence

From lowest to highest binding:

| Level | Operators | Associativity | Meaning |
| --- | --- | --- | --- |
| 1 | `\|>` | left | `x \|> f(a)` is `f(x, a)` |
| 2 | `??` | right | `x ?? d`: the value of an option, or `d` when it is `none` |
| 3 | `or` | left | Short-circuit boolean or |
| 4 | `and` | left | Short-circuit boolean and |
| 5 | `not` | prefix | Boolean not |
| 6 | `== != < <= > >= in is` | none | Comparison; `x in xs` membership; `x is Variant` enum test |
| 7 | `.. ..=` | none | Ranges (`0..n` excludes `n`) |
| 8 | `+ -` | left | Numbers, durations, money; `+` also joins strings and lists |
| 9 | `* / %` | left | `/` on `Int` truncates toward zero; division by zero is `Invalid` |
| 10 | `-` | prefix | Negation |
| 11 | `f(…)`, `f::<T>(…)`, `.name`, `?.name`, `[i]`, `with {…}` | postfix, left | Call, call with type arguments, field or method, optional chaining, index, call overrides |

Equality is structural on values. Handles (chapter 03) compare by identity. Comparison of
`Float` follows IEEE 754 (`NaN != NaN`); `list.sort` orders `NaN` last.

## Grammar

The grammar is EBNF (ISO 14977 style: `{ }` repetition, `[ ]` option, `|` choice, `( )`
grouping, terminals in double quotes). `IDENT`, `INT`, `QUANTITY`, `FLOAT`, `DURATION`, `MONEY`,
`STRING` and `RAW_STRING` are lexical tokens defined above (`STRING` includes its
interpolations); `;` includes inserted semicolons. `MEMBER` is an identifier or any keyword.

```ebnf
module          = [ meta_decl ] , { item } , EOF ;
meta_decl       = "meta" , record_lit , ";" ;

item            = { attribute } , [ "pub" ] , declaration , ";"
                | test_decl , ";" ;
declaration     = import_decl | type_decl | enum_decl | const_decl
                | fn_decl | harness_decl | prompt_decl | state_decl ;
attribute       = "@" , IDENT , [ "(" , [ arguments ] , ")" ] ;

import_decl     = "import" , import_path , [ "as" , IDENT ]
                , [ "{" , IDENT , { "," , IDENT } , "}" ] ;
import_path     = STRING | source_name ;
source_name     = [ IDENT , ":" ] , name_path , { "/" , name_path } , [ "@" , STRING ] ;
name_path       = IDENT , { "." , IDENT } ;
                  (* std:debate, user:critics.strict, plugin:acme/review @"^1", authored:session/vote *)

type_decl       = "type" , IDENT , [ type_params ] , "=" , type ;
enum_decl       = "enum" , IDENT , [ type_params ] , "{" , variant , { "," , variant } , [ "," ] , "}" ;
variant         = IDENT , [ record_type ] ;
type_params     = "<" , IDENT , { "," , IDENT } , ">" ;
const_decl      = "const" , IDENT , [ ":" , type ] , "=" , expr ;
fn_decl         = "fn" , IDENT , [ type_params ] , "(" , [ params ] , ")" , [ "->" , type ] , block ;
harness_decl    = "harness" , IDENT , "(" , [ params ] , ")" , [ "->" , type ]
                , [ "with" , record_lit ] , block ;
prompt_decl     = "prompt" , IDENT , "(" , [ params ] , ")" , prompt_body ;
state_decl      = "state" , ":" , type , "=" , expr ;
test_decl       = "test" , STRING , block ;
params          = param , { "," , param } , [ "," ] ;
param           = IDENT , ":" , type , [ "=" , expr ] ;

(* Types *)
type            = base_type , { "?" } ;
base_type       = type_name , [ "<" , type , { "," , type } , ">" ]
                | "[" , type , "]"                                   (* list *)
                | record_type
                | "(" , [ type , { "," , type } ] , ")"               (* tuple; () is Unit *)
                | "fn" , "(" , [ type , { "," , type } ] , ")" , "->" , type
                | "harness" , "(" , [ type , { "," , type } ] , ")" , "->" , type ;
type_name       = IDENT , { "." , IDENT } ;
record_type     = "{" , [ field_type , { "," , field_type } , [ "," ] ] , "}" ;
field_type      = { attribute } , IDENT , [ "?" ] , ":" , type , [ STRING ] ;   (* STRING: schema description *)

(* Blocks and statements *)
block           = "{" , { statement , ";" } , "}" ;
statement       = let_stmt | var_stmt | assign_stmt | defer_stmt | expr ;
let_stmt        = "let" , pattern , [ ":" , type ] , "=" , expr ;
var_stmt        = "var" , IDENT , [ ":" , type ] , "=" , expr ;
assign_stmt     = place , ( "=" | "+=" | "-=" | "*=" | "/=" ) , expr ;
place           = IDENT , { "." , MEMBER | "[" , expr , "]" } ;
defer_stmt      = "defer" , block ;

(* Expressions *)
expr            = jump_expr | pipe_expr ;
jump_expr       = "return" , [ expr ] | "break" , [ expr ] | "continue"
                | "raise" , expr ;
pipe_expr       = coalesce_expr , { "|>" , postfix_expr } ;
coalesce_expr   = or_expr , [ "??" , coalesce_expr ] ;
or_expr         = and_expr , { "or" , and_expr } ;
and_expr        = not_expr , { "and" , not_expr } ;
not_expr        = "not" , not_expr | compare_expr ;
compare_expr    = range_expr , [ compare_op , range_expr ] ;
compare_op      = "==" | "!=" | "<" | "<=" | ">" | ">=" | "in" | "is" ;
range_expr      = add_expr , [ ( ".." | "..=" ) , add_expr ] ;
add_expr        = mul_expr , { ( "+" | "-" ) , mul_expr } ;
mul_expr        = unary_expr , { ( "*" | "/" | "%" ) , unary_expr } ;
unary_expr      = "-" , unary_expr | postfix_expr ;
postfix_expr    = primary , { postfix } ;
postfix         = [ "::" , type_args ] , "(" , [ arguments ] , ")"
                | "." , MEMBER
                | "?." , MEMBER
                | "[" , expr , "]"
                | "with" , record_lit ;                  (* call overrides, chapter 04; same line *)
type_args       = "<" , type , { "," , type } , ">" ;
arguments       = positional , { "," , positional } , { "," , named } , [ "," ]
                | named , { "," , named } , [ "," ] ;
positional      = expr ;
named           = IDENT , ":" , expr ;

primary         = literal | IDENT | "(" , expr , ")" | tuple_lit | list_lit | record_lit
                | variant_lit | if_expr | match_expr | for_expr | while_expr | loop_expr
                | repeat_expr | try_expr | par_expr | race_expr | with_expr | do_expr
                | closure | prompt_expr ;
literal         = INT | QUANTITY | FLOAT | DURATION | MONEY | STRING | RAW_STRING
                | "true" | "false" | "none" ;
tuple_lit       = "(" , expr , "," , [ expr , { "," , expr } ] , ")" ;
list_lit        = "[" , [ list_item , { "," , list_item } , [ "," ] ] , "]" ;
list_item       = [ ".." ] , expr ;                       (* ..xs splices a list *)
record_lit      = "{" , [ field_init , { "," , field_init } , [ "," ] ] , "}" ;
field_init      = IDENT , ":" , expr | IDENT | ".." , expr ;   (* shorthand; spread *)
variant_lit     = type_name , record_lit ;                (* Denied { by: user }, Ok { value: v } *)

if_expr         = "if" , condition , block , [ "else" , ( if_expr | block ) ] ;
condition       = "let" , pattern , "=" , expr | expr ;
match_expr      = "match" , expr , "{" , match_arm , { "," , match_arm } , [ "," ] , "}" ;
match_arm       = pattern , [ "if" , expr ] , "=>" , expr ;
for_expr        = "for" , pattern , "in" , expr , block ;
while_expr      = "while" , condition , block ;
loop_expr       = "loop" , block ;
repeat_expr     = "repeat" , expr , block ;
try_expr        = "try" , block , "catch" , ( "{" , catch_arm , { "," , catch_arm } , [ "," ] , "}"
                                            | IDENT , block ) ;
catch_arm       = pattern , [ "if" , expr ] , "=>" , expr ;
par_expr        = "par" , "{" , expr , { ";" , expr } , [ ";" ] , "}" ;
race_expr       = "race" , "{" , expr , { ";" , expr } , [ ";" ] , "}" ;
with_expr       = "with" , "timeout" , expr , block , [ "else" , block ]
                | "with" , record_lit , block ;
do_expr         = "do" , block ;
closure         = "|" , [ closure_param , { "," , closure_param } ] , "|" , ( expr | block )
                | "||" , ( expr | block )
                | "fn" , "(" , [ params ] , ")" , [ "->" , type ] , block ;
closure_param   = alt_pattern , [ ":" , type ] ;          (* no top-level "|" alternatives *)

(* Patterns *)
pattern         = alt_pattern , { "|" , alt_pattern } ;
alt_pattern     = "_" | literal | IDENT , [ "@" , alt_pattern ]
                | type_name , [ record_pattern ]          (* enum variant, possibly qualified *)
                | "(" , pattern , "," , [ pattern , { "," , pattern } ] , ")"
                | "[" , [ list_pattern , { "," , list_pattern } ] , "]"
                | record_pattern ;
list_pattern    = pattern | ".." , [ IDENT ] ;
record_pattern  = "{" , [ field_pattern , { "," , field_pattern } ] , [ "," , ".." ] , "}" ;
field_pattern   = IDENT , [ ":" , pattern ] ;

(* Prompts, chapter 05 *)
prompt_expr     = "prompt" , prompt_body ;
prompt_body     = "{" , { section , ";" } , "}" ;
section         = "system" , expr
                | "instruct" , expr
                | "task" , expr
                | "data" , expr , [ "as" , expr ]
                | "image" , expr , [ "as" , expr ]
                | "example" , expr , "->" , expr
                | "format" , type
                | "steps" , expr
                | "context" , expr
                | "note" , expr
                | "include" , expr
                | "if" , expr , prompt_body , [ "else" , prompt_body ]
                | "for" , pattern , "in" , expr , prompt_body ;
```

### Disambiguation rules

1. **Blocks and records.** After a keyword that takes a block (`if`, `else`, `for`, `while`,
   `loop`, `repeat`, `try`, `catch`, `defer`, `do`, `fn`, `harness`, `test`) and after the head
   expression of `if c`, `while c`, `for p in xs`, `repeat n` and `with timeout d`, `{` starts a
   block; after `with` followed by a record, the first `{` is the configuration record and the
   next one the block. Elsewhere
   in expression position, including a closure body, `{` starts a **record literal** when the next
   token is `}` or `..`, or an identifier followed by `:` or `,`; otherwise it starts a block.
   So `|c| { steps: c.steps }` returns a record, `|c| { x }` is a block returning `x`, and a
   one-field shorthand record is written `{ x, }`. A record or variant literal in an `if`,
   `while`, `for`, `match`, `repeat` or `with timeout` head must be parenthesised.
2. **Type arguments** use the turbofish: `json.decode::<Plan>(j)`, `model.generate_as::<Note>(p)`.
   Bare `<` is always the comparison operator in expressions.
3. **Variant literals.** `Name { … }` in expression position (outside heads, rule 1) constructs
   the payload variant `Name`, which must resolve to a variant with exactly those fields.
4. **Block values.** A block's value is its last statement when that statement is an expression;
   otherwise `()` (Unit). Inserted semicolons do not change a block's value.
5. `|` begins a closure in primary position and separates patterns in a pattern; there is no
   bitwise-or operator. A closure parameter is a single pattern without top-level `|`.
6. **Patterns.** In a `match` or `catch` arm, an identifier that resolves to a variant of the
   scrutinee's enum (rule 10) or a qualified `type_name` naming a variant is a variant pattern;
   any other bare identifier binds.
7. `x with { … }` binds tighter than any binary operator, must start on the line of the call, and
   applies only to a call expression whose callee is a harness (chapter 04); on anything else it
   is a type error.
8. `in` inside `for … in` is the loop keyword, not the membership operator.
9. **Arguments.** Positional arguments come first, then named ones; each parameter is given at
   most once. A closure argument after named ones must be named (`par_map(xs, f: |x| …, limit: 4)`)
   or moved before them (`par_map(xs, |x| …, limit: 4)`).
10. **Contextual variants.** Where the expected type is an enum, a bare identifier that is not a
    **local** binding and names one of its unit variants resolves to that variant
    (`effort: high`, `context: fresh`, `mode: plan`); names from the prelude and imports do not
    block this, so `fail` matches `Verdict.fail` even though `fail(…)` is also a function. Where
    the expected type is `ToolSet` or `ToolName`, a bare identifier that is not a local binding
    names a tool (`tools: [read_file, grep]`). Everywhere else the qualified form is needed
    (`Effort.high`).

### Constants

A `const` initialiser is evaluated once, when the package is loaded. It may use literals, other
constants, operators, and calls of **pure** functions and pure built-ins (`model.route`,
`text.*`, `list.*`, …); the checker refuses calls that make effects (model calls, tools, the
clock, randomness). `const CRITIC: ModelRef = model.route(tier: frontier)` is therefore valid: it
builds a reference and resolves nothing until a call uses it.

## A complete small program

```ratio
meta { name: "summarise-dir", version: "0.2.0", description: "Summarise a folder's Rust files" }

/// One file's summary.
type FileNote = {
  path: Str,
  summary: Str "Two sentences at most",
  risky: Bool "True when the file has unsafe code or panics on input",
}

prompt summarise_file(path: Untrusted<Str>, body: Untrusted<Str>) {
  instruct "Summarise this Rust file for a reviewer."
  data body as path
  format FileNote
}

harness summarise(dir: Str) -> [Untrusted<FileNote>]
  with { tools: [glob, read_file], budget: { tokens: 200k, wall: 10m } }
{
  let files = tools.glob("*.rs", path: dir).paths |> list.take(40)
  par_map(files, |path| {
    let body = tools.read_file(path).text
    model.generate_as::<FileNote>(summarise_file(path, body))
  }, limit: 4)
  |> list.sort_by(|n| n.path)
}
```

The result is `[Untrusted<FileNote>]`: `path` and `summary` are model text and stay untrusted,
while `risky` is a `Bool` and is usable directly in conditions (chapter 03, taint).
