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

**Source.** UTF-8 text. A byte-order mark is ignored. Line ends are `\n` or `\r\n`. A carriage
return that is not followed by a line feed is a lexical error wherever it appears: in code, in a
comment, or in a string (write `\r` in a string for a carriage return character).

**Comments.** `//` to the end of the line. `///` is a documentation comment attached to the next
item or record field; `harnesses.list` shows it and schema derivation uses it as a field's
`description`. Any longer run of slashes starts a documentation comment too: `////` is one, and
its text begins with the fourth `/`.

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
| Duration | `500ms`, `30s`, `5m`, `2h` | `Duration` (whole microseconds, chapter 03); the literal must be a whole number of its unit |
| Money | `$0.50`, `$12` | `Money` (US dollars, exact to 1e-6) |
| Boolean | `true`, `false` | `Bool` |
| None | `none` | `T?` for any `T` |
| String | `"Hello, {name}!"` | `Str`, or `Untrusted<Str>` if an interpolated part is untrusted |
| Raw string | `r"C:\path\{not interpolated}"` | `Str` |
| Long string | `"""` … `"""` | `Str`; common indentation and the first and last blank lines are removed |
| Raw long string | `r"""` … `"""` | `Str`, no interpolation, same dedent |

**Numbers.** These rules are lexical; breaking one is an error of the token reader.

- **Digits.** Decimal digits, or hexadecimal after `0x` (lower-case `x`; the digits `0-9a-fA-F`).
  Leading zeros are allowed and mean nothing: `007` is 7 (there are no octal literals).
- **Separators.** `_` may appear between two digits, and only there: with a digit on both sides,
  in decimal and hexadecimal digits alike, in the whole part, the fraction and the exponent
  (`1_000`, `0xFF_FF`, `1_000.000_1`, `1e1_0`). `1_`, `1__0`, `_1` (an identifier), `1._5` and
  `1e_5` are not numbers with separators.
- **Floats** have a fraction (`.` followed by a digit), an exponent (`e` or `E`, an optional sign,
  digits), or both; the value must be finite. `1.` and `.5` are not floats, and `0..n` is a range.
- **Units.** A quantity (`k`, `M`) or duration (`ms`, `s`, `m`, `h`) unit follows the digits
  directly; any other letters after a number are an error. A quantity may have a fraction but no
  exponent, and must scale to a whole number (`1.5k` is 1,500; `1.0005k` is an error).
- **Durations** are whole numbers of their unit, so `1.5s` is a lexical error (write `1500ms`).
  The value is the number times the unit in **microseconds** (`500ms` is 500,000 µs), and it
  must fit in 64 bits, signed; the range is checked in microseconds.
- **Range.** An integer literal must fit in 64 bits, signed. The literal has no sign: `-x` is
  negation applied to `x`, so `-9223372036854775808` cannot be written (its literal is out of
  range before the negation); write `-9223372036854775807 - 1`.

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

- **Ends.** A short string ends at the first unescaped `"`. A long string ends at the **first**
  `"""`: up to two `"` in a row are text, and a long string cannot contain three (write `\"`
  for one of them; a raw long string cannot contain three at all). A line break ends a short
  string with an error.
- **Line ends.** In long strings and raw long strings a line end is part of the text, and a
  `\r\n` line end reads as `\n`; an escaped `\r` or `\n` is that character.
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

**Long-string dedent.** The text of a long string (raw or not) is cooked in these steps, after
escapes are decoded and with each interpolation kept as a placeholder:

1. Split the text into lines at its line ends. Only literal line ends split lines; an escaped
   `\n` does not.
2. If there are at least two lines, a first line holding only spaces and tabs (or nothing) is
   removed with the line end after it, and a last line holding only spaces and tabs is removed
   with the line end before it. So the rest of the opening `"""`'s line and the indentation
   before the closing `"""` are dropped when blank.
3. Of the remaining lines that are not blank (a line holding an interpolation is never blank),
   take the smallest number of leading indentation characters. Indentation is literal spaces and
   tabs, each counting as one character (tabs are not expanded); an escaped `\t` or `\u{20}` is
   never indentation, and an interpolation ends a line's indentation.
4. Remove up to that many leading indentation characters from every remaining line, blank or
   not, and join the lines with `\n`.

**Statement ends.** The grammar uses `;` to separate statements, and allows one after the last.
The parser's token reader inserts one at a line end when **all** of these hold:

1. the innermost open delimiter is a **block** or a **prompt body**, or there is none (the
   **top level of the file**); never `( )`, `[ ]`, an interpolation, a record literal or record
   type, an enum body, a `match` or `catch` arm list, or a declaration head from `fn`, `harness`
   or `prompt` up to the `{` of its body;
2. the line's last token is an identifier, a literal, `)`, `]`, `}`, `?`, or one of `break`,
   `continue`, `return`, `none`, `true`, `false`; or it is a `>` that closes a generic argument
   list, when the statement is a type declaration (it began with `type`) or a prompt's `format`
   section (see below);
3. the next non-blank line does not start with `.` (including `..` and `..=`), `?.`, `|>`, `??`,
   `and`, `or`, `else` or `catch`. Lines holding only a `//` comment count as blank; a line
   holding a `///` documentation comment does not.

One more case never inserts: a line that holds only **attributes** (`@turn`, `@allow(read_file)`,
one or several) does not end a statement, although its last token is an identifier or `)`. The
attribute belongs to the item on the lines below it. An attribute counts as such when its `@`
starts a statement or item (at the start of the file, or after `;`, `{` or another attribute);
it lasts through its name and, when present, its parenthesised arguments.

A `>` is otherwise never the last token of a statement, because it is the comparison operator.
Inside a type declaration (`type T = List<Int>`) and a prompt `format` section
(`format Map<K, V>`), a `>` that closes a `<` opened in the same statement ends the statement at
a line end, so the next item or section can start on the next line. The token reader does not
tell a generic `<` from a comparison, so it pairs each `>` with the nearest open `<` of the same
delimiter depth; in these two statements the grammar allows no comparison, so the pairing is
exact. Condition 3 still applies.

A block head (`if c`, `while c`, `for p in xs`, `repeat n`, `with timeout d`, and the heads of
`loop`, `try`, `defer`, `do`, `test`) must have its `{` on the same line: the line end after
the head's expression ends a statement as usual, and a `{` that then follows it is reported by
the parser as `P0008`, not read as a split head. A `fn`, `harness` or `prompt` head, which has
no expression to end a statement, may span lines up to its `{` (condition 1).

So pipelines and `else` on the next line continue an expression; inside a `match`, `catch`, enum
or record the line breaks are only whitespace and the entries are separated by commas; a
harness's `with` configuration and body may start on lines of their own. A `with` at the start of
a line in a block always begins a new `with` expression statement; a call override
(`f(x) with { … }`) must start on the call's line. The end of the file ends the last line.

**Operators** are read by maximal munch: the reader always takes the longest operator that the
next characters spell, and a token is never split afterwards. So `a<-b` reads `<` then `-`
(there is no `<-`), `a..=b` reads `..=`, and `T??` in a type is the single token `??`,
which the type grammar accepts in place of two `?` (by chapter 03 a nested option is the option,
so `T??` means `T?`). Where a type ends in `>` and `=` follows, a space is needed: `Map<K, V> = m`,
because `>=` is one token. There is no `>>` operator, so `[Map<Str, List<Int>>]` reads as two `>`.

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

The parser applies the grammar strictly. A trailing comma is accepted only where a production
shows `[ "," ]`; a `return`, `break`, `continue` or `raise` (a `jump_expr`) is an `expr`, so it
may be a statement, an argument or an arm body, but it is not an operand: `1 + return` and
`not break` are errors; and `()` is the unit type only, never an expression.

Parser nesting is the depth of the syntax tree: each nested expression, type or pattern counts
one level, and so does each prefix operator (`not`, `-`) and each link of a postfix, binary or
`|>` chain, so `a.b.c.d` is deeper than `a`. The limit is `max_nesting` (default 128, chapter
07); exceeding it is `P0002` and parsing stops.

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
type            = base_type , { "?" | "??" } ;                  (* "??" counts as two "?" *)
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
block           = "{" , [ statement , { ";" , statement } , [ ";" ] ] , "}" ;
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
record_pattern  = "{" , [ ( field_pattern , { "," , field_pattern } , [ "," , ".." ]
                          | ".." ) , [ "," ] ] , "}" ;
field_pattern   = IDENT , [ ":" , pattern ] ;

(* Prompts, chapter 05 *)
prompt_expr     = "prompt" , prompt_body ;
prompt_body     = "{" , [ section , { ";" , section } , [ ";" ] ] , "}" ;
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

1. **Blocks and records.** These rules decide what a `{` opens, in this order:
   - **Head keywords.** After a keyword that takes a block (`if`, `else`, `for`, `while`,
     `loop`, `repeat`, `try`, `catch`, `defer`, `do`, `fn`, `harness`, `test`) and after the head
     expression of `if c`, `while c`, `for p in xs`, `repeat n` and `with timeout d`, `{` starts a
     block. Directly after `catch`, `{` starts the arm list; after `match x` and `enum E`, the
     arm list and the enum body.
   - **`with` and a record.** After a `with` that begins an expression (not `with timeout`, not a
     call override after `)`, not a harness's configuration in its declaration head), the first
     `{` is the configuration record and the next one the block. In a declaration head,
     `with { … }` is the configuration record and the `{` after it the body.
   - **Variant literals.** Elsewhere in expression position, a `{` directly after a type name (an
     identifier spelled `UpperCamel`, possibly qualified as `Verdict.Fail`) starts a variant or
     record literal (rule 3), including the shorthand `Denied { by }`.
   - **Records.** Elsewhere in expression position, including a closure body, `{` starts a
     **record literal** when the next token is `}` or `..`, or an identifier followed by `:` or
     `,`; otherwise it starts a block. So `|c| { steps: c.steps }` returns a record,
     `|c| { x }` is a block returning `x`, and a one-field shorthand record is written `{ x, }`.

   A record or variant literal in an `if`, `while`, `for`, `match`, `repeat` or `with timeout`
   head must be parenthesised. The token reader needs these decisions for semicolon insertion
   before any parsing, so it approximates the parser with the keyword that led to the `{` plus
   this rule. The parser does not repeat the decision: it takes the brace kind the token reader
   attached to each `{` and `}` (the reader's decision is the rule's decision), and reports
   `P0003` when the kind does not fit the production being parsed.
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
