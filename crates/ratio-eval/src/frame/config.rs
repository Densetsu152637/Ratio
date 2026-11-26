//! Reading a configuration record (`HarnessConfig`, chapter 04) into a [`Request`] and the
//! [`Settings`] a host reads (model, effort, sampling, system prompt, title).
//!
//! The authority fields (`tools`, `permissions`, `budget`, `limits`, `context`, `sees`,
//! `attended`) become a [`Request`], which attenuation narrows; a value this module cannot
//! read exactly (a negative cap, a tool pattern with an inner `*`, a path with `..`) is
//! refused with `Invalid` rather than approximated.

use std::sync::Arc;

use super::scope::{
    BudgetKind, Context, Folder, Location, Mode, Request, SeesItem, SeesPatch, ToolPat,
    ToolRequest, ToolSet,
};
use crate::prepare::Prepared;
use crate::value::{Repr, Value};

/// The configuration a host reads to run a frame's model calls: inherited from the caller
/// where a frame does not set it. Each is the program's value (`ModelRef`, `Effort`,
/// `Thinking`, `Sampling`, `TrustedPrompt`, `Str`), with its labels.
#[derive(Clone, Debug, Default)]
pub struct Settings {
    pub model: Option<Value>,
    pub effort: Option<Value>,
    pub thinking: Option<Value>,
    pub sampling: Option<Value>,
    pub system: Option<Value>,
    pub title: Option<Value>,
}

impl Settings {
    /// `self` (a frame's own) over `outer` (the caller's or the declaration's), field by field.
    #[must_use]
    pub fn over(self, outer: &Settings) -> Settings {
        Settings {
            model: self.model.or_else(|| outer.model.clone()),
            effort: self.effort.or_else(|| outer.effort.clone()),
            thinking: self.thinking.or_else(|| outer.thinking.clone()),
            sampling: self.sampling.or_else(|| outer.sampling.clone()),
            system: self.system.or_else(|| outer.system.clone()),
            title: self.title.or_else(|| outer.title.clone()),
        }
    }
}

/// A field that is written (an absent optional field reads as `none`).
fn field(v: &Value, name: &str) -> Option<Value> {
    v.field(name).filter(|x| !x.is_none())
}

fn variant<'a>(p: &'a Prepared, v: &Value) -> Option<&'a str> {
    let (e, i) = match &v.repr {
        Repr::Variant(e, i, _) => (*e, *i),
        Repr::VariantP(d) => (d.enum_, d.index),
        _ => return None,
    };
    p.enums
        .get(e as usize)?
        .variants
        .get(i as usize)
        .map(|x| &*x.name)
}

fn count(v: &Value, what: &str) -> Result<u64, String> {
    let n = match &v.repr {
        Repr::Int(i) | Repr::Money(i) | Repr::Duration(i) => *i,
        _ => return Err(format!("`{what}` is not a number")),
    };
    u64::try_from(n).map_err(|_| format!("`{what}` is negative"))
}

fn small(v: &Value, what: &str) -> Result<u32, String> {
    let n = count(v, what)?;
    u32::try_from(n).map_err(|_| format!("`{what}` is too large"))
}

/// The most entries a configuration list (tools, paths, rules) may hold: the meets that open
/// a frame are quadratic in them, and charged for it.
pub const MAX_CONFIG_LIST: usize = 256;

fn texts(v: &Value, what: &str) -> Result<Vec<Arc<str>>, String> {
    let items = v
        .elements()
        .ok_or_else(|| format!("`{what}` is not a list"))?;
    super::scope::count(items.len() as u64);
    if items.len() > MAX_CONFIG_LIST {
        return Err(format!(
            "`{what}` holds {} entries, over the limit of {MAX_CONFIG_LIST}",
            items.len()
        ));
    }
    items
        .iter()
        .map(|x| match &x.repr {
            Repr::Str(s) | Repr::Tool(s) => Ok(Arc::from(s.as_str())),
            _ => Err(format!("`{what}` holds something other than text")),
        })
        .collect()
}

fn tool_set(v: &Value, what: &str) -> Result<ToolSet, String> {
    let names = texts(v, what)?;
    let mut pats = Vec::with_capacity(names.len());
    for n in names {
        pats.push(
            ToolPat::parse(&n)
                .ok_or_else(|| format!("`{n}` is not a tool name or a pattern ending in `*`"))?,
        );
    }
    Ok(ToolSet::of(pats))
}

fn tools(v: &Value) -> Result<ToolRequest, String> {
    if v.elements().is_some() {
        return Ok(ToolRequest {
            need: tool_set(v, "tools")?,
            want: ToolSet::none(),
        });
    }
    let need = field(v, "need").map_or(Ok(ToolSet::none()), |x| tool_set(&x, "tools.need"))?;
    let want = field(v, "want").map_or(Ok(ToolSet::none()), |x| tool_set(&x, "tools.want"))?;
    Ok(ToolRequest { need, want })
}

/// The work of reading a configuration's lists: one per entry and per 16 bytes of text, and
/// the cover filter over each list (quadratic in its length, up to the limit).
pub(crate) fn parse_work(cfg: &Value) -> u64 {
    let mut work = 0u64;
    let mut list = |v: Option<Value>| {
        if let Some(items) = v.and_then(|x| x.elements()) {
            let n = items.len() as u64;
            let bytes: u64 = items
                .iter()
                .map(|x| x.as_text().map_or(0, |t| t.len() as u64))
                .sum();
            work = work
                .saturating_add(n)
                .saturating_add(bytes.div_ceil(16))
                // A list over the limit is refused before its cover filter runs.
                .saturating_add(n.min(MAX_CONFIG_LIST as u64).pow(2));
        }
    };
    let tools = field(cfg, "tools");
    list(tools.clone());
    if let Some(t) = &tools {
        list(field(t, "need"));
        list(field(t, "want"));
    }
    if let Some(perm) = field(cfg, "permissions") {
        for name in ["deny", "ask", "paths"] {
            list(field(&perm, name));
        }
    }
    work
}

/// Reads a configuration record.
///
/// # Errors
///
/// A message for `Invalid` when a field cannot be read exactly.
pub(crate) fn read(p: &Prepared, cfg: &Value) -> Result<(Request, Settings), String> {
    let mut r = Request::default();
    if let Some(t) = field(cfg, "tools") {
        r.tools = Some(tools(&t)?);
    }
    if let Some(perm) = field(cfg, "permissions") {
        if let Some(m) = field(&perm, "mode") {
            let name = variant(p, &m).unwrap_or("");
            r.mode = Some(Mode::named(name).ok_or("an unknown permission mode")?);
        }
        if let Some(d) = field(&perm, "deny") {
            r.deny = texts(&d, "permissions.deny")?;
        }
        if let Some(a) = field(&perm, "ask") {
            r.ask = texts(&a, "permissions.ask")?;
        }
        if let Some(ps) = field(&perm, "paths") {
            let mut folders = Vec::new();
            for t in texts(&ps, "permissions.paths")? {
                folders.push(
                    Folder::parse(&t)
                        .ok_or_else(|| format!("the path `{t}` is not a plain folder"))?,
                );
            }
            r.paths = Some(folders);
        }
        if let Some(l) = field(&perm, "location") {
            let name = variant(p, &l).unwrap_or("");
            r.location = Some(Location::named(name).ok_or("an unknown location")?);
        }
    }
    if let Some(b) = field(cfg, "budget") {
        for k in BudgetKind::ALL {
            if let Some(x) = field(&b, k.name()) {
                r.budget.set(k, Some(count(&x, k.name())?));
            }
        }
    }
    if let Some(l) = field(cfg, "limits") {
        if let Some(d) = field(&l, "depth") {
            r.depth = Some(small(&d, "limits.depth")?);
        }
        if let Some(w) = field(&l, "width") {
            let w = small(&w, "limits.width")?;
            if w == 0 {
                return Err("`limits.width` must be at least 1".to_owned());
            }
            r.width = Some(w);
        }
        if let Some(h) = field(&l, "heap") {
            r.heap = Some(count(&h, "limits.heap")?);
        }
    }
    if let Some(c) = field(cfg, "context") {
        let name = variant(p, &c).unwrap_or("");
        r.context = Some(Context::named(name).ok_or("an unknown context mode")?);
    }
    if let Some(s) = field(cfg, "sees") {
        let mut patch = SeesPatch::default();
        for item in SeesItem::ALL {
            match field(&s, item.name()).and_then(|x| x.as_bool()) {
                Some(true) => patch.set = patch.set.with(item),
                Some(false) => patch.clear = patch.clear.with(item),
                None => {}
            }
        }
        r.sees = patch;
    }
    if let Some(a) = field(cfg, "attended") {
        r.attended = Some(a.as_bool().ok_or("`attended` is not a Bool")?);
    }
    let settings = Settings {
        model: field(cfg, "model"),
        effort: field(cfg, "effort"),
        thinking: field(cfg, "thinking"),
        sampling: field(cfg, "sampling"),
        system: field(cfg, "system"),
        title: field(cfg, "title"),
    };
    Ok((r, settings))
}
