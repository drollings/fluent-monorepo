//! Bytecode representation tests: every instruction round-trips through
//! the wire form, malformed bytes refuse by shape, and the test-only
//! disassembler output is pinned exactly.
//!
//! Test path note: this file rides the `pine::bytecode` unit module, so
//! the runnable filter is `pine::bytecode`, not the file stem.

use super::*;
use crate::vars::NodeId;

fn full_coverage_block(pool: &mut ConstPool, symbols: &mut SymbolTable) -> CompiledBlock {
    let float = pool.intern(ConstValue::Float(1.5));
    let title = pool.intern(ConstValue::Str("A".to_owned()));
    let site = pool.intern(ConstValue::Str("plot@a".to_owned()));
    let tag = pool.intern(ConstValue::Str("plot@tag".to_owned()));
    let call_site = symbols.intern("test@[]");
    let lib = symbols.intern("showmethegrail/libMAs");
    let smooth = symbols.intern("smooth");
    let helper = symbols.intern("helper");
    let x = symbols.intern("x");
    let mut block = CompiledBlock::new();
    block.push(Instruction::Const {
        dst: SlotId(0),
        value: float,
    });
    block.push(Instruction::Copy {
        dst: SlotId(1),
        src: SlotId(0),
    });
    block.push(Instruction::BinOp {
        dst: SlotId(2),
        op: BinOp::Add,
        lhs: SlotId(0),
        rhs: SlotId(1),
    });
    block.push(Instruction::UnOp {
        dst: SlotId(3),
        op: UnOp::Negate,
        src: SlotId(2),
    });
    block.push(Instruction::Builtin {
        dst: SlotId(4),
        var: BuiltinVarKind::Close,
    });
    // Every chart-scope kind rides its own tag; each one must round-trip
    // like the leg/bar-index kinds it composes with.
    for (index, var) in [
        BuiltinVarKind::ChartSymbol,
        BuiltinVarKind::ChartPeriod,
        BuiltinVarKind::ChartIsIntraday,
        BuiltinVarKind::ChartTimezone,
        BuiltinVarKind::ChartPointValue,
        BuiltinVarKind::ChartMinContract,
        BuiltinVarKind::BarstateIsFirst,
        BuiltinVarKind::BarstateIsNew,
        BuiltinVarKind::BarstateIsConfirmed,
        BuiltinVarKind::BarstateIsLast,
        BuiltinVarKind::BarstateIsHistory,
        BuiltinVarKind::BarstateIsRealtime,
        BuiltinVarKind::Timenow,
        BuiltinVarKind::Tr,
        BuiltinVarKind::ChartPeriodSeconds,
    ]
    .into_iter()
    .enumerate()
    {
        block.push(Instruction::Builtin {
            dst: SlotId(30 + index as u32),
            var,
        });
    }
    block.push(Instruction::History {
        dst: SlotId(5),
        src: SlotId(4),
        offset: -3,
    });
    block.push(Instruction::Call {
        dst: Some(SlotId(6)),
        target: CallTarget::Block(1),
        args: vec![
            CallArgSlot {
                name: None,
                slot: SlotId(4),
            },
            CallArgSlot {
                name: None,
                slot: SlotId(5),
            },
        ],
        homes: vec![None, None],
        type_args: vec![],
        site: call_site,
    });
    block.push(Instruction::Call {
        dst: None,
        target: CallTarget::Named {
            module: Some(lib),
            name: smooth,
        },
        args: vec![],
        homes: vec![],
        type_args: vec![],
        site: call_site,
    });
    block.push(Instruction::Call {
        dst: Some(SlotId(7)),
        target: CallTarget::Named {
            module: None,
            name: helper,
        },
        args: vec![CallArgSlot {
            name: Some(x),
            slot: SlotId(6),
        }],
        homes: vec![Some(x)],
        type_args: vec![CallTypeArg {
            name: symbols.intern("display"),
            args: vec![CallTypeArg {
                name: symbols.intern("all"),
                args: vec![],
            }],
        }],
        site: call_site,
    });
    block.push(Instruction::JumpIfFalse {
        cond: SlotId(7),
        target: 12,
        ctx: CondContext::If,
    });
    block.push(Instruction::Plot {
        title: PlotTitle::Slot(SlotId(30)),
        src: SlotId(2),
        site,
        display: None,
    });
    block.push(Instruction::Jump { target: 13 });
    block.push(Instruction::Return { src: None });
    block.push(Instruction::Return {
        src: Some(SlotId(2)),
    });
    block.push(Instruction::GStore {
        slot: SlotId(9),
        src: SlotId(2),
        plot_tag: Some(tag),
    });
    block.push(Instruction::Coalesce {
        dst: SlotId(10),
        options: vec![SlotId(2), SlotId(7)],
    });
    block.push(Instruction::PlotCandle {
        open: SlotId(0),
        high: SlotId(1),
        low: SlotId(2),
        close: SlotId(4),
        title: Some(title),
        site,
        display: None,
        color: Some(SlotId(10)),
        wickcolor: None,
        bordercolor: None,
    });
    block.push(Instruction::BarColor {
        src: SlotId(2),
        title: None,
        site,
    });
    block.push(Instruction::PlotShape {
        src: SlotId(7),
        title: Some(title),
        site,
    });
    block.push(Instruction::Hline {
        src: SlotId(0),
        title: None,
        site,
    });
    block.push(Instruction::Fill {
        first: FillLeg::PlotSite(site),
        second: FillLeg::Variable(symbols.intern("paint")),
        title: None,
        site,
        color: Some(SlotId(10)),
    });
    block.push(Instruction::Fallback {
        reason: FallbackReason::CaptureStatement {
            kind: CaptureKind::Strategy,
        },
        node: NodeId::new(41),
    });
    block.push(Instruction::Fallback {
        reason: FallbackReason::CaptureShape {
            kind: CaptureKind::Fill,
        },
        node: NodeId::new(42),
    });
    block.push(Instruction::GStoreLoop {
        slot: SlotId(9),
        src: SlotId(2),
    });
    block.push(Instruction::ForInit {
        first: SlotId(0),
        last: SlotId(2),
        counter: SlotId(11),
        step: SlotId(12),
        ascending: SlotId(13),
        stride: SlotId(14),
    });
    block.push(Instruction::WhileTick { trip: SlotId(14) });
    block.push(Instruction::ForInInit {
        arr: SlotId(15),
        len: SlotId(16),
    });
    block.push(Instruction::ArrayGet {
        dst: SlotId(17),
        arr: SlotId(15),
        idx: SlotId(0),
        hist: Some(SlotId(4)),
    });
    block.push(Instruction::ArrayNew {
        dst: SlotId(18),
        options: vec![SlotId(0), SlotId(2)],
    });
    block.push(Instruction::SwitchEq {
        dst: SlotId(19),
        target: SlotId(0),
        probe: SlotId(2),
    });
    block.push(Instruction::Destructure {
        dsts: vec![SlotId(20), SlotId(21)],
        arr: SlotId(15),
    });
    block.push(Instruction::JournalWrite {
        home: symbols.intern("g"),
        src: SlotId(2),
        mirror: SlotId(22),
        strict: false,
    });
    block.push(Instruction::PlotTag {
        name: symbols.intern("p"),
        tag: None,
    });
    block.push(Instruction::VarDecl {
        ordinal: 0,
        is_varip: false,
        src: SlotId(2),
        name: symbols.intern("c"),
    });
    block.push(Instruction::VarStore {
        ordinal: 0,
        is_varip: true,
        src: SlotId(2),
        name: symbols.intern("c"),
        strict: true,
    });
    block.push(Instruction::VarRead {
        dst: SlotId(23),
        ordinal: 0,
        name: symbols.intern("c"),
        hist: Some(-1),
    });
    block.push(Instruction::FieldGet {
        dst: SlotId(24),
        obj: SlotId(15),
        field: symbols.intern("n"),
    });
    block.push(Instruction::ObjNew {
        dst: SlotId(25),
        type_name: symbols.intern("Box2"),
        fields: vec![(symbols.intern("n"), SlotId(2))],
    });
    let site_args = pool.intern(ConstValue::Str(format!("{:?}", Vec::<String>::new())));
    block.push(Instruction::InstanceCall {
        dst: Some(SlotId(26)),
        recv: SlotId(25),
        method: symbols.intern("bump"),
        args: vec![CallArgSlot {
            name: None,
            slot: SlotId(2),
        }],
        homes: vec![None],
        args_debug: site_args,
        home: Some(CallHome::Var {
            name: symbols.intern("w"),
            var_ordinal: None,
        }),
        stmt: false,
    });
    block.push(Instruction::ThisRead { dst: SlotId(27) });
    block.push(Instruction::ThisFieldGet {
        dst: SlotId(28),
        field: symbols.intern("n"),
    });
    block.push(Instruction::ThisFieldSet {
        field: symbols.intern("n"),
        src: SlotId(2),
    });
    block.push(Instruction::AliasRead {
        dst: SlotId(29),
        name: symbols.intern("level"),
        var_ordinal: None,
    });
    block.push(Instruction::AliasFieldGet {
        dst: SlotId(30),
        name: symbols.intern("level"),
        field: symbols.intern("n"),
        var_ordinal: Some(0),
    });
    block.push(Instruction::FieldPut {
        home: CallHome::Var {
            name: symbols.intern("w"),
            var_ordinal: None,
        },
        field: symbols.intern("n"),
        src: SlotId(2),
    });
    block.push(Instruction::Fallback {
        reason: FallbackReason::BuiltinStatement {
            namespace: symbols.intern("array"),
            method: symbols.intern("shift"),
        },
        node: NodeId::new(43),
    });
    block.push(Instruction::LocalHistory {
        dst: SlotId(31),
        slot: SlotId(2),
        name: symbols.intern("x"),
        offset: HistOffset::Const(1),
        bare: None,
    });
    // Nested/dotted homes and the map-entry/variable provenance pair.
    block.push(Instruction::InstanceCall {
        dst: Some(SlotId(32)),
        recv: SlotId(2),
        method: symbols.intern("bump"),
        args: Vec::new(),
        homes: Vec::new(),
        args_debug: pool.intern(ConstValue::Str("[]".to_owned())),
        home: Some(CallHome::VarPath {
            name: symbols.intern("outer"),
            var_ordinal: None,
            path: vec![symbols.intern("inner")],
        }),
        stmt: false,
    });
    block.push(Instruction::InstanceCall {
        dst: Some(SlotId(33)),
        recv: SlotId(2),
        method: symbols.intern("touch"),
        args: Vec::new(),
        homes: Vec::new(),
        args_debug: pool.intern(ConstValue::Str("[]".to_owned())),
        home: Some(CallHome::ThisField {
            field: symbols.intern("level"),
        }),
        stmt: false,
    });
    block.push(Instruction::FieldPut {
        home: CallHome::ThisField {
            field: symbols.intern("level"),
        },
        field: symbols.intern("n"),
        src: SlotId(2),
    });
    block.push(Instruction::DottedGet {
        dst: SlotId(34),
        root: SlotId(2),
        name: symbols.intern("held.n"),
        field: symbols.intern("n"),
        flat: None,
    });
    // Request plan: every dataset-tail shape plus the merge/count operands.
    block.push(Instruction::RequestSecurity {
        dst: Some(SlotId(36)),
        symbol: SlotId(40),
        timeframe: SlotId(41),
        gaps: Some(SlotId(42)),
        lookahead: Some(SlotId(43)),
        calc_bars: Some(SlotId(44)),
        single: false,
        items: vec![
            SecurityExpr::Leg(symbols.intern("close")),
            SecurityExpr::Const(pool.intern(ConstValue::Float(1.5))),
            SecurityExpr::Call {
                method: symbols.intern("sma"),
                source: Some(symbols.intern("volume")),
                chart_args: vec![SlotId(45), SlotId(46)],
            },
            SecurityExpr::Call {
                method: symbols.intern("atr"),
                source: None,
                chart_args: vec![SlotId(47)],
            },
            SecurityExpr::Binary {
                op: BinOp::Multiply,
                left: Box::new(SecurityExpr::Leg(symbols.intern("high"))),
                right: Box::new(SecurityExpr::Const(pool.intern(ConstValue::Int(2)))),
            },
        ],
        site: symbols.intern("request.security"),
    });
    block.push(Instruction::RequestLowerTf {
        dst: Some(SlotId(37)),
        symbol: SlotId(40),
        timeframe: SlotId(41),
        legs: vec![
            symbols.intern("time"),
            symbols.intern("open"),
            symbols.intern("close"),
        ],
        calc_bars: Some(SlotId(44)),
        site: symbols.intern("request.security_lower_tf"),
    });
    block.push(Instruction::StrategyRead {
        dst: SlotId(35),
        member: symbols.intern("initial_capital"),
    });
    block.push(Instruction::SetMapProv {
        name: symbols.intern("held"),
        tag: Some((symbols.intern("m"), SlotId(2))),
    });
    block.push(Instruction::SetMapProv {
        name: symbols.intern("held"),
        tag: None,
    });
    block.push(Instruction::Refuse {
        reason: pool.intern(ConstValue::Str("unknown function 'foo'".to_owned())),
    });
    block.push(Instruction::Refuse {
        reason: pool.intern(ConstValue::Str("unknown function 'foo'".to_owned())),
    });
    block.push(Instruction::RuntimeRefuse { message: SlotId(9) });
    block.push(Instruction::Noop);
    // Every remaining variant, one each: the round-trip gate below asserts
    // this fixture covers the whole wire form, so a new instruction cannot
    // skip the codec.
    block.push(Instruction::GLoad {
        dst: SlotId(38),
        slot: SlotId(4),
    });
    block.push(Instruction::GStoreLoop {
        slot: SlotId(4),
        src: SlotId(0),
    });
    block.push(Instruction::GStore {
        slot: SlotId(4),
        src: SlotId(0),
        plot_tag: Some(tag),
    });
    block.push(Instruction::GReassign {
        slot: SlotId(4),
        src: SlotId(0),
        plot_tag: None,
    });
    block.push(Instruction::GStoreInit {
        slot: SlotId(5),
        src: SlotId(0),
    });
    block.push(Instruction::JumpIfNa {
        cond: SlotId(0),
        target: 1,
    });
    block.push(Instruction::Call {
        dst: Some(SlotId(39)),
        target: CallTarget::Resolved {
            module: 0,
            block: 1,
        },
        args: vec![CallArgSlot {
            name: None,
            slot: SlotId(0),
        }],
        homes: vec![None],
        type_args: Vec::new(),
        site: call_site,
    });
    block.push(Instruction::PlotCandle {
        open: SlotId(0),
        high: SlotId(1),
        low: SlotId(1),
        close: SlotId(0),
        title: Some(tag),
        site: tag,
        display: None,
        color: Some(SlotId(0)),
        wickcolor: None,
        bordercolor: Some(SlotId(1)),
    });
    block.push(Instruction::BarColor {
        src: SlotId(0),
        title: Some(tag),
        site: tag,
    });
    block.push(Instruction::PlotShape {
        src: SlotId(0),
        title: Some(tag),
        site: tag,
    });
    block.push(Instruction::Hline {
        src: SlotId(0),
        title: Some(tag),
        site: tag,
    });
    block.push(Instruction::Fill {
        first: FillLeg::PlotSite(tag),
        second: FillLeg::Variable(x),
        title: Some(tag),
        site: tag,
        color: Some(SlotId(1)),
    });
    block.push(Instruction::Coalesce {
        dst: SlotId(2),
        options: vec![SlotId(0), SlotId(1)],
    });
    block.push(Instruction::ForInit {
        first: SlotId(0),
        last: SlotId(1),
        counter: SlotId(2),
        step: SlotId(1),
        ascending: SlotId(2),
        stride: SlotId(3),
    });
    block.push(Instruction::WhileTick { trip: SlotId(0) });
    block.push(Instruction::ForInInit {
        arr: SlotId(0),
        len: SlotId(1),
    });
    block.push(Instruction::ArrayGet {
        dst: SlotId(2),
        arr: SlotId(0),
        idx: SlotId(1),
        hist: Some(SlotId(3)),
    });
    block.push(Instruction::ArrayNew {
        dst: SlotId(2),
        options: vec![SlotId(0), SlotId(1)],
    });
    block.push(Instruction::Destructure {
        dsts: vec![SlotId(0), SlotId(1)],
        arr: SlotId(2),
    });
    block.push(Instruction::JournalWrite {
        home: x,
        src: SlotId(0),
        mirror: SlotId(1),
        strict: true,
    });
    block.push(Instruction::PlotTag {
        name: x,
        tag: Some(tag),
    });
    block.push(Instruction::VarDecl {
        ordinal: 1,
        is_varip: false,
        src: SlotId(0),
        name: x,
    });
    block.push(Instruction::VarStore {
        ordinal: 1,
        is_varip: true,
        src: SlotId(0),
        name: x,
        strict: false,
    });
    block.push(Instruction::VarRead {
        dst: SlotId(2),
        ordinal: 1,
        name: x,
        hist: Some(1),
    });
    block.push(Instruction::FieldGet {
        dst: SlotId(2),
        obj: SlotId(0),
        field: x,
    });
    block.push(Instruction::ObjNew {
        dst: SlotId(2),
        type_name: x,
        fields: vec![(x, SlotId(0))],
    });
    block.push(Instruction::InstanceCall {
        dst: Some(SlotId(2)),
        recv: SlotId(0),
        method: smooth,
        args: vec![CallArgSlot {
            name: None,
            slot: SlotId(0),
        }],
        homes: vec![None],
        args_debug: tag,
        home: Some(CallHome::Innermost),
        stmt: false,
    });
    block.push(Instruction::ThisRead { dst: SlotId(2) });
    block.push(Instruction::ThisFieldGet {
        dst: SlotId(2),
        field: x,
    });
    block.push(Instruction::ThisFieldSet {
        field: x,
        src: SlotId(0),
    });
    block.push(Instruction::AliasRead {
        dst: SlotId(2),
        name: x,
        var_ordinal: Some(1),
    });
    block.push(Instruction::AliasFieldGet {
        dst: SlotId(2),
        name: x,
        field: x,
        var_ordinal: None,
    });
    block.push(Instruction::FieldPut {
        home: CallHome::Innermost,
        field: x,
        src: SlotId(0),
    });
    block.push(Instruction::SwitchEq {
        dst: SlotId(2),
        target: SlotId(0),
        probe: SlotId(1),
    });
    block.push(Instruction::DottedGet {
        dst: SlotId(2),
        root: SlotId(0),
        name: x,
        field: x,
        flat: Some(SlotId(7)),
    });
    block.push(Instruction::EnumMark {
        enm: x,
        members: vec![x],
    });
    block.push(Instruction::EnumRead {
        dst: SlotId(2),
        enm: x,
        member: x,
        name: x,
        flat: SlotId(7),
    });
    block.push(Instruction::AliasFlatRead {
        dst: SlotId(2),
        flat: SlotId(7),
        src: SlotId(0),
    });
    block.push(Instruction::StrategyRead {
        dst: SlotId(2),
        member: x,
    });
    block.push(Instruction::SetMapProv {
        name: x,
        tag: Some((x, SlotId(0))),
    });
    block.push(Instruction::Fallback {
        reason: FallbackReason::Loops,
        node: NodeId::new(3),
    });

    block
}

#[test]
fn every_instruction_round_trips() {
    let mut pool = ConstPool::new();
    let mut symbols = SymbolTable::new();
    let block = full_coverage_block(&mut pool, &mut symbols);
    let bytes = encode_block(&block, &symbols);
    let mut decoded_symbols = SymbolTable::new();
    assert_eq!(
        decode_block(&bytes, &mut decoded_symbols).expect("round trip decodes"),
        block,
        "encode/decode is the identity over every variant"
    );
    assert_eq!(
        decoded_symbols, symbols,
        "symbol tables round-trip in encounter order"
    );
}

#[test]
fn const_pool_round_trips_every_kind() {
    let mut pool = ConstPool::new();
    let mut symbols = SymbolTable::new();
    let display = symbols.intern("display");
    let all = symbols.intern("all");
    pool.intern(ConstValue::Float(0.5));
    pool.intern(ConstValue::Int(-7));
    pool.intern(ConstValue::Int(i64::MIN));
    pool.intern(ConstValue::Int(i64::MAX));
    pool.intern(ConstValue::Bool(true));
    pool.intern(ConstValue::Str("plot@a".to_owned()));
    pool.intern(ConstValue::Color(0xFF_00_00_00));
    pool.intern(ConstValue::Color(u32::MAX));
    pool.intern(ConstValue::Enum {
        namespace: display,
        member: all,
    });
    pool.intern(ConstValue::Na);
    // Interning deduplicates: the pool holds ten entries, ids are stable.
    assert_eq!(pool.len(), 10);
    assert_eq!(
        pool.intern(ConstValue::Float(0.5)),
        ConstId(0),
        "repeat interning reuses the id"
    );
    let bytes = encode_consts(&pool, &symbols);
    let mut decoded_symbols = SymbolTable::new();
    assert_eq!(
        decode_consts(&bytes, &mut decoded_symbols).expect("pool round trip decodes"),
        pool,
        "const pools round-trip every kind"
    );
    assert_eq!(
        decoded_symbols, symbols,
        "enum names round-trip through the table"
    );
}

#[test]
fn symbol_tables_round_trip() {
    let mut symbols = SymbolTable::new();
    let first = symbols.intern("ta.sma@[a]");
    assert_eq!(
        symbols.intern("ta.sma@[a]"),
        first,
        "repeat interning reuses the id"
    );
    symbols.intern("helper");
    let bytes = encode_symbols(&symbols);
    assert_eq!(
        decode_symbols(&bytes).expect("table round trip decodes"),
        symbols,
        "symbol tables round-trip"
    );
}

#[test]
fn malformed_bytes_refuse_by_shape() {
    let mut pool = ConstPool::new();
    let mut symbols = SymbolTable::new();
    let bytes = encode_block(&full_coverage_block(&mut pool, &mut symbols), &symbols);
    let mut probe = SymbolTable::new();
    // Truncation at every prefix refuses (never a partial block).
    for cut in [0, 1, 3, 7, bytes.len() / 2, bytes.len() - 1] {
        assert!(
            decode_block(&bytes[..cut], &mut probe).is_err(),
            "prefix of {cut} bytes refuses"
        );
    }
    // Unknown tag refuses: count one, tag 0xFF.
    assert_eq!(
        decode_block(&[1, 0, 0, 0, 0xFF], &mut SymbolTable::new()),
        Err(DecodeError::UnknownTag(0xFF)),
        "unknown tags refuse by value"
    );
    // Trailing bytes refuse instead of decoding a prefix.
    let mut trailed = bytes.clone();
    trailed.push(0x00);
    assert_eq!(
        decode_block(&trailed, &mut SymbolTable::new()),
        Err(DecodeError::TrailingBytes(1)),
        "trailing bytes refuse with their count"
    );
}

#[test]
fn disassembly_shape_is_pinned() {
    // The golden shape of `a = close + 1.5; plot(a)`: reviewed output,
    // pinned here so disassembler drift fails loudly.
    let mut pool = ConstPool::new();
    let symbols = SymbolTable::new();
    let one_five = pool.intern(ConstValue::Float(1.5));
    let site = pool.intern(ConstValue::Str("plot@a".to_owned()));
    let mut block = CompiledBlock::new();
    block.push(Instruction::Builtin {
        dst: SlotId(0),
        var: BuiltinVarKind::Close,
    });
    block.push(Instruction::Const {
        dst: SlotId(1),
        value: one_five,
    });
    block.push(Instruction::BinOp {
        dst: SlotId(2),
        op: BinOp::Add,
        lhs: SlotId(0),
        rhs: SlotId(1),
    });
    block.push(Instruction::Plot {
        title: PlotTitle::Slot(SlotId(30)),
        src: SlotId(2),
        site,
        display: None,
    });
    assert_eq!(
        disassemble(&block, &pool, &symbols),
        "0000 builtin s0 <- Close\n\
         0001 const s1 <- Some(Float(1.5))\n\
         0002 binop s2 <- s0 Add s1\n\
         0003 plot s30 <- s2 @ \"plot@a\"\n",
        "disassembler output pinned"
    );
    // The const-title shape disassembles through the pool on its own
    // line: same capture, statically named series.
    let const_title = pool.intern(ConstValue::Str("T".to_owned()));
    let mut legacy = CompiledBlock::new();
    legacy.push(Instruction::Plot {
        title: PlotTitle::Const(const_title),
        src: SlotId(2),
        site,
        display: None,
    });
    assert_eq!(
        disassemble(&legacy, &pool, &symbols),
        "0000 plot \"T\" <- s2 @ \"plot@a\"\n",
        "the const-title shape has its own pinned line"
    );
    // The computed-refusal shape disassembles through its message slot on
    // its own line: same run-stopping contract, run-time wording.
    let mut refusing = CompiledBlock::new();
    refusing.push(Instruction::RuntimeRefuse { message: SlotId(9) });
    assert_eq!(
        disassemble(&refusing, &pool, &symbols),
        "0000 runtime_refuse s9\n",
        "the computed-refusal shape has its own pinned line"
    );
    // The checked-rebind shape disassembles like a store with its own
    // verb: same tag discipline, run-time declaredness check.
    let mut rebind = CompiledBlock::new();
    rebind.push(Instruction::GReassign {
        slot: SlotId(4),
        src: SlotId(0),
        plot_tag: None,
    });
    assert_eq!(
        disassemble(&rebind, &pool, &symbols),
        "0000 greassign g4 <- s0\n",
        "the checked-rebind shape has its own pinned line"
    );
    // The flat-first dotted shape names its write-back column after the
    // field path: same read, flat wins when recorded.
    let mut symbols = SymbolTable::new();
    let mut dotted = CompiledBlock::new();
    dotted.push(Instruction::DottedGet {
        dst: SlotId(2),
        root: SlotId(0),
        name: symbols.intern("b.v"),
        field: symbols.intern("v"),
        flat: Some(SlotId(9)),
    });
    assert_eq!(
        disassemble(&dotted, &pool, &symbols),
        "0000 dotted s2 <- s0 (b.v . v) flat=g9\n",
        "the flat-first dotted shape has its own pinned line"
    );
}

#[test]
fn legacy_plot_bytes_decode_to_const_title() {
    // Tag `0x0B` keeps its documented meaning: a title constant. Bytes
    // laid out by the pre-change encoder decode to the const-title shape,
    // never the evaluated-slot form.
    let bytes = vec![
        1, 0, 0, 0,    // one instruction
        0x0B, // plot, const title
        4, 0, 0, 0, // title ConstId(4)
        2, 0, 0, 0, // src SlotId(2)
        7, 0, 0, 0, // site ConstId(7)
        0, // display None
    ];
    let mut symbols = SymbolTable::new();
    let mut expected = CompiledBlock::new();
    expected.push(Instruction::Plot {
        title: PlotTitle::Const(ConstId(4)),
        src: SlotId(2),
        site: ConstId(7),
        display: None,
    });
    assert_eq!(
        decode_block(&bytes, &mut symbols).expect("legacy bytes decode"),
        expected,
        "tag 0x0B decodes the const-title shape"
    );
}

#[test]
fn both_plot_forms_round_trip() {
    // Each title shape survives its own tag: the const form under `0x0B`,
    // the evaluated-slot form under `0x35`. Neither tag decodes to the
    // other's meaning.
    let symbols = SymbolTable::new();
    let mut const_block = CompiledBlock::new();
    const_block.push(Instruction::Plot {
        title: PlotTitle::Const(ConstId(1)),
        src: SlotId(2),
        site: ConstId(3),
        display: None,
    });
    let mut slot_block = CompiledBlock::new();
    slot_block.push(Instruction::Plot {
        title: PlotTitle::Slot(SlotId(9)),
        src: SlotId(2),
        site: ConstId(3),
        display: Some(SlotId(4)),
    });
    let const_bytes = encode_block(&const_block, &symbols);
    let slot_bytes = encode_block(&slot_block, &symbols);
    assert_eq!(const_bytes[4], 0x0B, "the const form keeps its tag");
    assert_eq!(slot_bytes[4], 0x35, "the slot form carries the new tag");
    assert_eq!(
        decode_block(&const_bytes, &mut SymbolTable::new()).expect("const form decodes"),
        const_block,
        "the const form round-trips"
    );
    assert_eq!(
        decode_block(&slot_bytes, &mut SymbolTable::new()).expect("slot form decodes"),
        slot_block,
        "the slot form round-trips"
    );
}

#[test]
fn fallback_reasons_render_through_the_table() {
    // Typed reasons render the same human text the coverage output
    // reports, resolved through the owning table.
    let mut symbols = SymbolTable::new();
    let name = symbols.intern("superTrend");
    let reason = FallbackReason::HistoryUndeclared { name };
    assert_eq!(
        reason.display(&symbols),
        "history over undeclared name \"superTrend\" runs on the tree-walk",
        "payloads render by id"
    );
    assert_eq!(
        FallbackReason::Loops.display(&symbols),
        "loops run on the tree-walk (unlowered bounds and break semantics)",
        "unit variants render fixed text"
    );
}

#[test]
fn enum_kinds_parse_and_index() {
    // Leg and namespace categorization is total over the reserved set
    // and closed over unknown names.
    for (name, index) in [
        ("open", 0),
        ("high", 1),
        ("low", 2),
        ("close", 3),
        ("volume", 4),
        ("time", 5),
        ("hl2", 6),
        ("hlc3", 7),
        ("ohlc4", 8),
        ("wcl", 9),
    ] {
        let kind = LegKind::parse(name).expect("reserved leg parses");
        assert_eq!(kind.index(), index, "{name} has its column");
        assert_eq!(LegKind::from_index(index), Some(kind), "index inverts");
        assert_eq!(kind.name(), name, "names invert");
    }
    assert_eq!(LegKind::parse("ohlc3"), None, "no such leg");
    for (name, namespace) in [
        ("ta", BuiltinNamespace::Ta),
        ("input", BuiltinNamespace::Input),
        ("math", BuiltinNamespace::Math),
        ("str", BuiltinNamespace::Str),
        ("color", BuiltinNamespace::Color),
        ("timeframe", BuiltinNamespace::Timeframe),
        ("core", BuiltinNamespace::Core),
        ("array", BuiltinNamespace::Array),
        ("matrix", BuiltinNamespace::Matrix),
        ("map", BuiltinNamespace::Map),
        ("strategy", BuiltinNamespace::Strategy),
    ] {
        assert_eq!(BuiltinNamespace::parse(name), Some(namespace));
        assert_eq!(namespace.as_str(), name);
    }
    assert_eq!(BuiltinNamespace::parse("request"), None);
    assert_eq!(
        CaptureKind::parse("plotcandle"),
        Some(CaptureKind::PlotCandle)
    );
    assert_eq!(CaptureKind::parse("plot"), None);
}

#[test]
fn round_trip_fixture_covers_every_instruction() {
    // The gate behind `every_instruction_round_trips`: the fixture must
    // actually carry one of each variant. `Instruction::wire_name` is
    // exhaustive by construction, so a new variant fails to compile until
    // it is named, and this test then fails until it is covered.
    let mut pool = ConstPool::new();
    let mut symbols = SymbolTable::new();
    let block = full_coverage_block(&mut pool, &mut symbols);
    let covered: std::collections::HashSet<&str> =
        block.instrs.iter().map(Instruction::wire_name).collect();
    let missing: Vec<&str> = INSTRUCTION_NAMES
        .iter()
        .copied()
        .filter(|name| !covered.contains(name))
        .collect();
    assert!(
        missing.is_empty(),
        "the round-trip fixture must cover every variant, missing: {missing:?}"
    );
    let unknown: Vec<&&str> = covered
        .iter()
        .filter(|name| !INSTRUCTION_NAMES.contains(name))
        .collect();
    assert!(
        unknown.is_empty(),
        "every fixture name belongs to the variant list: {unknown:?}"
    );
}
