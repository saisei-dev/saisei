from compiler.disassemble import compute_xrefs
from compiler.ir_to_c import build_basic_blocks
from compiler import cfg


def test_iret_no_fallthrough_xrefs():
    insns = [
        {
            "address": 0,
            "bytes": "CF",
            "mnemonic": "iret",
            "op_str": "",
            "detail": {},
        }
    ]
    xrefs = compute_xrefs(insns)
    assert all("fallthrough@" not in x for x in xrefs)


def test_cfg_no_fallthrough_after_iret():
    instrs = [
        {"address": 0x0, "mnemonic": "iret", "op_str": "", "bytes": "CF"},
        {"address": 0x1, "mnemonic": "nop", "op_str": "", "bytes": "90"},
    ]
    blocks = build_basic_blocks(instrs)
    graph = cfg.build_cfg(blocks)
    assert graph.out_degree(0x0) == 0
