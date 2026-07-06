import sys
"""Tests for additional flag-affecting mnemonics."""

# flake8: noqa

import sys
from pathlib import Path

import networkx as nx
import pytest

sys.path.append(str(Path(__file__).resolve().parents[1]))

from compiler.ir_to_c import normalize_flags, propagate_flag_conditions  # noqa: E402
from compiler.basic_block import BasicBlock  # noqa: E402


@pytest.mark.parametrize(
    "mnemonic,op", [
        ("add", "al, 1"),
        ("sub", "al, 1"),
        ("xor", "al, al"),
        ("adc", "al, 1"),
        ("sbb", "al, 1"),
        ("shl", "ax, 1"),
        ("shr", "ax, 1"),
        ("sal", "ax, 1"),
        ("sar", "ax, 1"),
        ("rol", "al, 1"),
        ("ror", "al, 1"),
        ("rcl", "al, 1"),
        ("rcr", "al, 1"),
    ]
)
def test_normalize_flags_attaches_previous(mnemonic, op):
    instrs = [
        {"address": 0, "mnemonic": mnemonic, "op_str": op, "bytes": ""},
        {"address": 2, "mnemonic": "je", "op_str": "0005", "bytes": ""},
    ]
    result = normalize_flags(instrs)
    assert result[1]["cond_prev"]["mnemonic"] == mnemonic


@pytest.mark.parametrize(
    "mnemonic,op", [
        ("add", "al, 1"),
        ("sub", "al, 1"),
        ("xor", "al, al"),
        ("adc", "al, 1"),
        ("sbb", "al, 1"),
        ("shl", "ax, 1"),
        ("shr", "ax, 1"),
        ("sal", "ax, 1"),
        ("sar", "ax, 1"),
        ("rol", "al, 1"),
        ("ror", "al, 1"),
        ("rcl", "al, 1"),
        ("rcr", "al, 1"),
    ]
)
def test_propagate_flag_conditions_across_blocks(mnemonic, op):
    pred = BasicBlock(0)
    pred.instructions = [
        {"address": 0, "mnemonic": mnemonic, "op_str": op, "bytes": ""}
    ]
    succ = BasicBlock(2)
    succ.instructions = [
        {"address": 2, "mnemonic": "je", "op_str": "0005", "bytes": ""}
    ]
    graph = nx.DiGraph()
    graph.add_edge(pred.start, succ.start)
    propagate_flag_conditions({pred.start: pred, succ.start: succ}, graph)
    assert succ.instructions[0]["cond_prev"]["mnemonic"] == mnemonic
