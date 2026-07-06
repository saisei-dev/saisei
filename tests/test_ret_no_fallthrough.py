from compiler.disassemble import compute_xrefs


def test_ret_no_fallthrough_uppercase():
    insns = [
        {
            "address": 0,
            "bytes": "c3",
            "mnemonic": "RET",
            "op_str": "",
            "detail": {},
        }
    ]
    xrefs = compute_xrefs(insns)
    assert all("fallthrough@" not in x for x in xrefs)
