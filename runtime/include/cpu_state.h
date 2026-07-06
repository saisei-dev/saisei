#ifndef CPU_STATE_H
#define CPU_STATE_H

#include <stdint.h>

/* 8086 register representation */
typedef union {
    uint16_t x;
    struct {
        uint8_t l;
        uint8_t h;
    } byte;
} Register16;

/* CPU state with general purpose registers and flags */
typedef struct {
    /* General purpose registers. The field names are prefixed to avoid
     * colliding with the convenience macros below (e.g. `ax`, `al`, etc.).
     * Without the prefix, macros like `#define al (cpu.ax.byte.l)` would
     * expand the `ax` token inside the structure path, producing invalid C
     * such as `cpu.(cpu.ax.x).byte.l`.
     */
    Register16 r_ax;
    Register16 r_bx;
    Register16 r_cx;
    Register16 r_dx;

    uint16_t si;
    uint16_t di;
    uint16_t bp;
    uint16_t sp;
    uint16_t r_ip;
    /* Segment registers.  These are also prefixed to avoid macro expansion
     * issues similar to the general purpose registers above.  Without the
     * prefix, references like `cpu.cs` would expand the `cs` macro below and
     * yield invalid code such as `cpu.(cpu.cs)`. */
    uint16_t r_cs;
    uint16_t r_ds;
    uint16_t r_es;
    uint16_t r_ss;
    struct {
        uint8_t CF;
        uint8_t PF;
        uint8_t ZF;
        uint8_t SF;
        uint8_t OF;
        uint8_t IF;
        uint8_t DF;
    } flags;
} CPUState;

extern CPUState cpu;

/* Macros to access registers and flags */
#define ax (cpu.r_ax.x)
#define al (cpu.r_ax.byte.l)
#define ah (cpu.r_ax.byte.h)
#define bx (cpu.r_bx.x)
#define bl (cpu.r_bx.byte.l)
#define bh (cpu.r_bx.byte.h)
#define cx (cpu.r_cx.x)
#define cl (cpu.r_cx.byte.l)
#define ch (cpu.r_cx.byte.h)
#define dx (cpu.r_dx.x)
#define dl (cpu.r_dx.byte.l)
#define dh (cpu.r_dx.byte.h)
#define si (cpu.si)
#define di (cpu.di)
#define bp (cpu.bp)
#define sp (cpu.sp)
#define ip (cpu.r_ip)
#define cs (cpu.r_cs)
#define ds (cpu.r_ds)
#define es (cpu.r_es)
#define ss (cpu.r_ss)
#define CF (cpu.flags.CF)
#define PF (cpu.flags.PF)
#define ZF (cpu.flags.ZF)
#define SF (cpu.flags.SF)
#define OF (cpu.flags.OF)
#define IF (cpu.flags.IF)
#define DF (cpu.flags.DF)

#endif /* CPU_STATE_H */
