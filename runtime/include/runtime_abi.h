#pragma once
/* ===========================================================================
 * runtime_abi.h — the CONTRACT between generated code and the runtime.
 *
 * The translator (scripts/ir_to_c.py) lowers each DOS binary to C under
 * the artifacts tree. That generated code is only allowed to call into the
 * hand-written runtime (scripts/shims.c) through the fixed surface listed
 * below. Keeping that surface explicit is what lets the two sides evolve
 * independently: a change to runtime internals that does not touch a symbol
 * here cannot break generated code, and the translator cannot start depending
 * on a new runtime entry point without it being declared here on purpose.
 *
 * Two parts of the ABI are genuinely standalone headers and are pulled in
 * directly so this file is a self-contained description of the contract:
 *   - cpu_state.h   : the 8086 register/flag globals (ax, bx, CF, ZF, ...)
 *   - rcb_fields.h  : the RCBField enum used by the rcb_read / rcb_write fns
 *
 * The function/macro declarations for the call surface currently live in
 * shims.h (which #includes this file); physically relocating them here is a
 * follow-up. What is authoritative TODAY is the machine-readable symbol
 * manifest below: it is the single source of truth for "what may generated
 * code call", and it is enforced by tests/test_runtime_abi_contract.py, which
 * fails if the translator emits a call to anything not listed here.
 *
 * To add a runtime entry point for generated code: declare it in shims.h AND
 * add its name to the manifest below (and the contract test will keep you
 * honest).
 * ===========================================================================
 */

#include "cpu_state.h"
#include "rcb_fields.h"

/* RUNTIME_ABI_SYMBOLS_BEGIN
 *   --- control / bookkeeping inserted around every translated instruction
 *   SAFEPOINT shim_log
 *   --- segmented memory access
 *   memb memw memb_write memw_write memb_raw seg_off
 *   --- arithmetic / flag helpers
 *   parity8 xor8 xor16
 *   --- control transfer (call/jmp/ret families) and interrupts
 *   call_table lcall_table jump_table near_ret_tail long_jump
 *   run_interrupt schedule_interrupt iret retf retf_pop
 *   --- per-binary lifecycle + cross-binary tail dispatch
 *   shim_enter_binary shim_leave_binary
 *   shim_tail_dispatch_save shim_tail_dispatch_restore
 *   shim_drain_pending_tail_dispatch
 *   --- block string operations
 *   rep_movsb_block rep_movsw_block rep_stosb_block shim_swap_regions_w
 *   scanMemoryForAl compareMemoryUntilMismatch
 *   --- resident control block fields
 *   rcb_read8 rcb_write8 rcb_read16 rcb_write16
 *   --- IO ports
 *   inb inw outb outw
 *   --- DOS (INT 21h) services
 *   dos_api dos_exit dos_get_version dos_print_string dos_write_char
 *   dos_direct_console_io dos_read_file dos_write_file dos_open_file
 *   dos_close_file dos_close_fcb dos_lseek dos_reset_disk
 *   dos_set_interrupt_vector dos_get_interrupt_vector
 *   dos_alloc_mem dos_free_mem dos_exec
 *   dos_get_file_attributes dos_delete_file dos_ioctl dos_get_current_dir
 *   dos_resize_mem dos_create_file dos_get_dta dos_find_next dos_flush_and_read
 *   dos_get_current_drive dos_find_first
 *   dos_get_time dos_get_date dos_console_input_no_echo
 *   dos_change_dir dos_make_dir dos_set_dta dos_select_drive
 *   dos_check_keyboard_status dos_get_disk_free_space
 *   --- BIOS services (INT 10h video / INT 16h keyboard)
 *   bios_set_video_mode bios_set_palette bios_set_cga_palette
 *   bios_video_alt_select
 *   bios_set_cursor_position bios_teletype_output bios_keyboard
 *   bios_display_combination_code bios_display_combination_alt_code
 *   bios_read_char_attr bios_write_char_attr bios_write_char_only
 *   bios_get_cursor bios_current_video_mode bios_current_video_columns
 *   bios_current_active_page
 * RUNTIME_ABI_SYMBOLS_END */
