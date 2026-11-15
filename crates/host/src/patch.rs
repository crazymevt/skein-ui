//! Translate UI protocol patch vectors into engine operations.

use norn_ui_engine::{Data, Op, Props};

type Result<T> = std::result::Result<T, String>;

fn id(d: &Data, what: &str) -> Result<u64> {
    match d {
        Data::Num(n) if *n >= 1.0 && n.fract() == 0.0 => Ok(*n as u64),
        _ => Err(format!("{what} must be a positive integer id, got {d:?}")),
    }
}

fn index(d: &Data) -> Result<usize> {
    match d {
        Data::Num(n) if *n >= 0.0 && n.fract() == 0.0 => Ok(*n as usize),
        _ => Err(format!("expected an index, got {d:?}")),
    }
}

fn props(d: &Data) -> Result<Props> {
    norn_ui_engine::style::props_of(d)
}

/// One op from its vector form, e.g. `[:create 3 :button {:on #{:click}}]`.
pub fn parse_op(d: &Data) -> Result<Op> {
    let xs = d
        .as_list()
        .ok_or_else(|| format!("An op is a vector, got {d:?}"))?;
    let name = xs
        .first()
        .and_then(Data::as_key)
        .ok_or_else(|| format!("An op starts with a keyword: {d:?}"))?;
    let arg = |i: usize| {
        xs.get(i)
            .ok_or_else(|| format!("[:{name} …] needs more arguments: {d:?}"))
    };
    Ok(match name {
        "hello" => Op::Hello(index(arg(1)?)? as u32),
        "window" => Op::Window {
            id: id(arg(1)?, "a window")?,
            props: props(arg(2)?)?,
        },
        "close-window" => Op::CloseWindow(id(arg(1)?, "a window")?),
        "create" => Op::Create {
            id: id(arg(1)?, "a node")?,
            kind: arg(2)?
                .as_key()
                .ok_or_else(|| "a node kind is a keyword".to_owned())?
                .to_owned(),
            props: xs.get(3).map(props).transpose()?.unwrap_or_default(),
        },
        "set" => Op::Set {
            id: id(arg(1)?, "a node")?,
            props: props(arg(2)?)?,
        },
        "style" => Op::Style {
            id: id(arg(1)?, "a node")?,
            style: props(arg(2)?)?,
        },
        "text" => Op::Text {
            id: id(arg(1)?, "a node")?,
            text: arg(2)?
                .as_str()
                .ok_or_else(|| "[:text id string]".to_owned())?
                .to_owned(),
        },
        "insert" => Op::Insert {
            parent: id(arg(1)?, "a parent")?,
            index: index(arg(2)?)?,
            id: id(arg(3)?, "a node")?,
        },
        "move" => Op::Move {
            parent: id(arg(1)?, "a parent")?,
            from: index(arg(2)?)?,
            to: index(arg(3)?)?,
        },
        "remove" => Op::Remove(id(arg(1)?, "a node")?),
        "theme" => Op::Theme(arg(1)?.clone()),
        "focus" => Op::Focus(id(arg(1)?, "a node")?),
        "stats" => Op::Stats(index(arg(1)?)? as u64),
        "menu" | "context-menu" | "dialog" | "clipboard-set" | "clipboard-get" => Op::Platform {
            op: name.to_owned(),
            args: xs[1..].to_vec(),
        },
        "line-count" | "lines" | "carets" | "decorations" | "reveal" | "invalidate" => Op::Editor {
            id: id(arg(1)?, "an editor")?,
            op: name.to_owned(),
            args: xs[2..].to_vec(),
        },
        other => return Err(format!("Unknown UI op :{other}")),
    })
}
