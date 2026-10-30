//! Headless `strategy.*` capture: one state owner and one dispatcher.
//!
//! The harness observes the broker dialogue a script asks for — entries,
//! exits, closes — as ordered [`StrategyCall`] rows, and answers the
//! sizing/account reads a script needs while it decides. Nothing here
//! submits, fills, or tracks a position: the run has no venue, so the book
//! is empty by construction and the recorded rows are the only output.
//!
//! Both engines call this module. The tree-walk evaluates arguments with
//! the surrounding scope; the compiled loop reads them from its frame. The
//! argument values then meet here, where one implementation owns the
//! declaration capital, the empty-book answers, and every refusal wording.
//! A live-only member (anything outside the harness matrix) refuses
//! exactly as the tree-walk does and records nothing.

use super::builtins_core::CoreArg;
use super::eval::StrategyCall;
use crate::value::PineValue;
use crate::KernelError;

/// Members the harness answers on a plain `strategy.<member>` read.
const READ_MEMBERS: &str = "long, short, fixed, commission.percent, initial_capital, closedtrades, opentrades, wintrades, losstrades, position_size, equity, netprofit, openprofit, grossprofit, grossloss, position_avg_price, max_drawdown";

/// Calls the harness records or answers.
const CALL_MEMBERS: &str =
    "entry, exit, close, close_all, closedtrades.size, closedtrades.exit_comment";

/// Every `strategy.*` store that lives exactly as long as one run.
#[derive(Debug, Default)]
pub(crate) struct StrategyState {
    /// Declared initial capital, once the harness declaration ran. Both
    /// `initial_capital` and `equity` read it; before it exists both read
    /// as a refusal, never as an invented default.
    initial_capital: Option<f64>,
    /// Recorded broker dialogue in execution order.
    calls: Vec<StrategyCall>,
}

impl StrategyState {
    /// Run the harness declaration: retain the declared capital and drop
    /// every other broker setting (pyramiding, commission, slippage, and
    /// margin are documented harness boundaries). Arguments have already
    /// evaluated, so only the capital value is inspected here.
    pub(crate) fn declare(&mut self, args: &[CoreArg]) -> Result<(), KernelError> {
        for arg in args {
            if arg.name.as_deref() != Some("initial_capital") {
                continue;
            }
            match &arg.value {
                Some(PineValue::Int(value)) => self.initial_capital = Some(*value as f64),
                Some(PineValue::Float(value)) => self.initial_capital = Some(*value),
                _ => return Err(refused("strategy initial_capital must be a number")),
            }
        }
        Ok(())
    }

    /// Drain the recorded dialogue into the run outcome.
    pub(crate) fn take_calls(&mut self) -> Vec<StrategyCall> {
        std::mem::take(&mut self.calls)
    }

    /// One `strategy.<member>` read. Direction and sizing constants answer
    /// literally, the empty-book history reads zero or `na`, and a member
    /// outside the matrix refuses naming itself. The empty book is the
    /// documented harness boundary: fills never happen here, so there is no
    /// position, trade, or profit to report.
    pub(crate) fn read(&self, member: &str) -> Result<Option<PineValue>, KernelError> {
        match member {
            "long" => Ok(Some(PineValue::Str("long".to_owned()))),
            "short" => Ok(Some(PineValue::Str("short".to_owned()))),
            "fixed" => Ok(Some(PineValue::Str("contracts".to_owned()))),
            "commission.percent" => Ok(Some(PineValue::Str("percent".to_owned()))),
            "initial_capital" => self.capital_or_refuse("initial_capital"),
            // No broker runs headlessly, so the book is empty by
            // construction: counts read zero, realized money reads zero,
            // and anything that needs a fill reads `na`. The one modeled
            // exception is `equity`, which reads the initial capital so
            // sizing can compute a quantity — fills are still never
            // booked, positions never tracked, and `closedtrades` stays
            // zero, so the harness observes entries but never their
            // aftermath.
            "closedtrades" | "opentrades" | "wintrades" | "losstrades" => {
                Ok(Some(PineValue::Int(0)))
            }
            "position_size" => Ok(Some(PineValue::Float(0.0))),
            "netprofit" | "grossprofit" | "grossloss" | "commission" => {
                Ok(Some(PineValue::Float(0.0)))
            }
            "equity" => self.capital_or_refuse("equity"),
            "position_avg_price" | "openprofit" | "max_drawdown" => Ok(None),
            _ => Err(refused(format!(
                "strategy.{member} is outside the harness matrix (Supported: {READ_MEMBERS})"
            ))),
        }
    }

    /// Read the declaration capital, refusing when the declaration has not
    /// run yet. `member` names the read that refused, so `equity` and
    /// `initial_capital` keep their own wordings.
    fn capital_or_refuse(&self, member: &str) -> Result<Option<PineValue>, KernelError> {
        self.initial_capital
            .map(PineValue::Float)
            .map(Some)
            .ok_or_else(|| {
                refused(format!(
                    "strategy.{member} read before any strategy declaration ran"
                ))
            })
    }

    /// One `strategy.<member>(...)` call on `bar`: record the broker
    /// dialogue, answer the matrix calls, and refuse anything live-only.
    /// `member` is the namespace tail plus the method (`closedtrades.size`).
    pub(crate) fn invoke(
        &mut self,
        bar: usize,
        member: &str,
        args: &[CoreArg],
    ) -> Result<Option<PineValue>, KernelError> {
        match member {
            "declare" => {
                self.declare(args)?;
                Ok(None)
            }
            "entry" => {
                let direction = match positional(args, 1) {
                    Some(PineValue::Str(value)) => Some(value),
                    _ => None,
                };
                let id = match positional(args, 0) {
                    Some(PineValue::Str(value)) => Some(value),
                    _ => None,
                }
                .or_else(|| text(args, &["id"]));
                self.calls.push(StrategyCall {
                    bar,
                    kind: "entry",
                    id: id.unwrap_or_default(),
                    direction,
                    qty: number(args, &["qty"]),
                    qty_percent: number(args, &["qty_percent"]),
                    limit: number(args, &["limit"]),
                    stop: number(args, &["stop"]),
                    profit: None,
                    loss: None,
                    comment: text(args, &["comment", "comment_profit", "comment_loss"]),
                    from_entry: None,
                });
                Ok(None)
            }
            "exit" => {
                self.calls.push(StrategyCall {
                    bar,
                    kind: "exit",
                    id: text(args, &["id"]).unwrap_or_default(),
                    direction: None,
                    qty: number(args, &["qty"]),
                    qty_percent: number(args, &["qty_percent"]),
                    limit: number(args, &["limit"]),
                    stop: number(args, &["stop"]),
                    profit: number(args, &["profit"]),
                    loss: number(args, &["loss"]),
                    comment: text(args, &["comment", "comment_profit", "comment_loss"]),
                    from_entry: text(args, &["from_entry"]),
                });
                Ok(None)
            }
            "close" => {
                self.calls.push(StrategyCall {
                    bar,
                    kind: "close",
                    id: text(args, &["id"]).unwrap_or_default(),
                    direction: None,
                    qty: number(args, &["qty"]),
                    qty_percent: number(args, &["qty_percent"]),
                    limit: None,
                    stop: None,
                    profit: None,
                    loss: None,
                    comment: text(args, &["comment"]),
                    from_entry: None,
                });
                Ok(None)
            }
            "close_all" => {
                self.calls.push(StrategyCall {
                    bar,
                    kind: "close_all",
                    id: String::new(),
                    direction: None,
                    qty: None,
                    qty_percent: None,
                    limit: None,
                    stop: None,
                    profit: None,
                    loss: None,
                    comment: text(args, &["comment"]),
                    from_entry: None,
                });
                Ok(None)
            }
            "closedtrades.size" => Ok(Some(PineValue::Int(0))),
            "closedtrades.exit_comment" => Ok(None),
            _ => Err(refused(format!(
                "strategy call strategy.{member} is outside the harness matrix (Supported: {CALL_MEMBERS})"
            ))),
        }
    }
}

/// The `index`-th positional argument's value. Positions count absent
/// arguments too (a missing `na` keeps later positions from shifting),
/// exactly like the tree-walk's evaluated list.
fn positional(args: &[CoreArg], index: usize) -> Option<PineValue> {
    args.iter()
        .filter(|arg| arg.name.is_none())
        .nth(index)
        .and_then(|arg| arg.value.clone())
}

/// First named argument among `names` read as a number.
fn number(args: &[CoreArg], names: &[&str]) -> Option<f64> {
    args.iter().find_map(|arg| {
        let key = arg.name.as_deref()?;
        if !names.contains(&key) {
            return None;
        }
        match &arg.value {
            Some(PineValue::Float(value)) => Some(*value),
            Some(PineValue::Int(value)) => Some(*value as f64),
            _ => None,
        }
    })
}

/// First named argument among `names` read as text.
fn text(args: &[CoreArg], names: &[&str]) -> Option<String> {
    args.iter().find_map(|arg| {
        let key = arg.name.as_deref()?;
        if !names.contains(&key) {
            return None;
        }
        match &arg.value {
            Some(PineValue::Str(value)) => Some(value.clone()),
            _ => None,
        }
    })
}

/// One refusal wording for the whole module: the tree-walk and the loop
/// share it by construction.
fn refused(message: impl Into<String>) -> KernelError {
    KernelError::Eval(message.into())
}
