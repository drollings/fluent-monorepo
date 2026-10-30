#[test]
fn dbg11_families() {
    use crate::pine::bytecode::Instruction;
    let scripts = [
        "type Box\n    float v = 0.0\nid(x) => x\nb = Box.new(3.0)\ny = id(b.v)\nplot(y, title=\"V\")",
        "type Counter\n    float n = 0.0\npeek(x) => x * 10.0\nmethod go(Counter this) =>\n    v = peek(this.n)\n    this.n := v\n    this.n\nc = Counter.new(1.0)\ny = c.go()\nplot(y, title=\"V\")",
        "m = map.new<string, float>()\nmap.put(m, \"k\", 1.0)\nh = map.get(m, \"k\")\nid(x) => x\ny = id(h)\nplot(y + map.get(m, \"k\"), title=\"V\")",
    ];
    for script in scripts {
        let program = crate::parse_program(script).expect("parses");
        let enums = crate::pine::bytecode_compile::EnumRegistry::build(&[("local/Driver", &program, &std::collections::HashMap::new())]);
        let decls = crate::pine::bytecode_compile::DeclRegistry::build(&[("local/Driver", &program)]);
        let exports = crate::pine::bytecode_compile::ExportRegistry::build(&[("local/Driver", &program)]);
        let module = crate::pine::bytecode_compile::compile_module("local/Driver", &program, &std::collections::HashMap::new(), &enums, &decls, &exports);
        for block in &module.blocks {
            for instr in &block.instrs {
                if let Instruction::Fallback { reason, .. } = instr {
                    eprintln!("SCRIPT {script:?}\n  -> {} : {}", reason.wire_name(), reason.display(&module.symbols));
                }
            }
        }
        eprintln!("SCRIPT done: {script:?}");
    }
}
