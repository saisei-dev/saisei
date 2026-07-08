/* Resident Control Block field layout. Hand-maintained single source of
 * truth for the RCB: compiled into the runtime (shims.c) and parsed by
 * scripts/the source to name RCB accesses in generated code. Edit here. */
#ifndef RCB_FIELDS_H
#define RCB_FIELDS_H

typedef enum {
    FIELD_1 = 0xFF00,
    PROGRAM_SEG = 0xFF02,
    PREV_TIMER_VECTOR_OFF = 0xFF04,
    PREV_TIMER_VECTOR_SEG = 0xFF06,
    FIELD_5 = 0xFF08,
    FIELD_6 = 0xFF09,
    JOYSTICK_FLAG = 0xFF0A,
    FIELD_8 = 0xFF0B,
    DATA_BUF1_OFF = 0xFF0C,
    DATA_BUF1_SEG = 0xFF0E,
    DATA_BUF2_OFF = 0xFF10,
    DATA_BUF2_SEG = 0xFF12,
    VIDEO_DRIVER_INDEX = 0xFF14,
    MUSIC_DRIVER_FLAG = 0xFF15,
    FIELD_15 = 0xFF16,
    FIELD_16 = 0xFF17,
    FIELD_17 = 0xFF18,
    FIELD_18 = 0xFF1D,
    FIELD_19 = 0xFF1E,
    FIELD_20 = 0xFF1F,
    FIELD_21 = 0xFF26,
    FIELD_22 = 0xFF27,
    FIELD_23 = 0xFF28,
    DATA_BASE_SEG = 0xFF2C,
    FIELD_25 = 0xFF33,
    FIELD_26 = 0xFF34,
    FIELD_27 = 0xFF38,
    FIELD_28 = 0xFF39,
    FIELD_29 = 0xFF3A,
    FIELD_30 = 0xFF3B,
    FIELD_31 = 0xFF3C,
    FIELD_32 = 0xFF40,
    FIELD_33 = 0xFF42,
    FIELD_34 = 0xFF43,
    FIELD_35 = 0xFF74,
    FIELD_36 = 0xFF75,
    FIELD_37 = 0xFF78,
    PREV_KEYBOARD_VECTOR_OFF = 0xFF79,
    PREV_KEYBOARD_VECTOR_SEG = 0xFF7B
} RCBField;

#endif /* RCB_FIELDS_H */
