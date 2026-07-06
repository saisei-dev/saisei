// virtual_display_sdl.c
#include <SDL.h>
#include <stdbool.h>
#include <stddef.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

#include "virtual_display.h"
#include "shims.h"   // for shim_keyboard_enqueue* and virtual_display_buffer
#include "mouse.h"   // for mouse_host_motion / mouse_host_button (INT 33h)
#include "save_manager.h"

static SDL_Window   *window;
static SDL_Renderer *renderer;
static SDL_Texture  *texture;
static bool          display_ready;

static uint8_t *rgb_buffer;
static int      rgb_buffer_size;

static int      win_w, win_h;   // logical size (game pixels)
static int      scale_hint = 3; // default window scale
static int      texture_w, texture_h;

// ----- helpers -----

static inline uint8_t vga6_to_8(uint8_t v6) {
  // Expand 6-bit DAC component to 8-bit: replicate high bits
  return (uint8_t)((v6 << 2) | (v6 >> 4));
}

static void ensure_rgb_buffer(int w, int h) {
  const int need = w * h * 3;
  if (rgb_buffer_size >= need) return;
  void *tmp = realloc(rgb_buffer, need);
  if (!tmp) {
    fprintf(stderr, "virtual_display: out of memory (%d bytes)\n", need);
    return;
  }
  rgb_buffer = (uint8_t*)tmp;
  rgb_buffer_size = need;
}

static void recreate_texture(int w, int h) {
  if (!renderer || w <= 0 || h <= 0) return;
  if (texture && w == texture_w && h == texture_h) return;

  if (texture) {
    SDL_DestroyTexture(texture);
    texture = NULL;
  }

  texture = SDL_CreateTexture(renderer, SDL_PIXELFORMAT_RGB24,
                              SDL_TEXTUREACCESS_STREAMING, w, h);
  if (!texture) {
    fprintf(stderr, "SDL_CreateTexture failed: %s\n", SDL_GetError());
    texture_w = texture_h = 0;
    display_ready = false;
    return;
  }

  SDL_RenderSetLogicalSize(renderer, w, h);
  if (window) {
    SDL_SetWindowSize(window, w * scale_hint, h * scale_hint);
  }

  texture_w = w;
  texture_h = h;
  display_ready = true;
}

static bool     pressed_scancodes[128];
static bool     ctrl_pressed;

struct ScancodeMapping {
  SDL_Scancode sdl_scancode;
  uint8_t      dos_scancode;
};

static const struct ScancodeMapping scancode_mappings[] = {
    {SDL_SCANCODE_UP,    0x48},
    {SDL_SCANCODE_DOWN,  0x50},
    {SDL_SCANCODE_LEFT,  0x4B},
    {SDL_SCANCODE_RIGHT, 0x4D},
};

static void release_pressed_scancodes(void) {
  for (int i = 0; i < (int)(sizeof(pressed_scancodes) / sizeof(pressed_scancodes[0]));
       ++i) {
    if (pressed_scancodes[i]) {
      pressed_scancodes[i] = false;
      shim_keyboard_enqueue_scancode_release((uint8_t)i);
    }
  }
}

static void mark_scancode_pressed(uint8_t scancode) {
  scancode &= 0x7F;
  if (!scancode) {
    return;
  }
  if (!pressed_scancodes[scancode]) {
    pressed_scancodes[scancode] = true;
    shim_keyboard_enqueue_scancode_press(scancode);
  }
}

static void mark_scancode_released(uint8_t scancode) {
  scancode &= 0x7F;
  if (!scancode) {
    return;
  }
  if (pressed_scancodes[scancode]) {
    pressed_scancodes[scancode] = false;
    shim_keyboard_enqueue_scancode_release(scancode);
  }
}

/* Enqueue a press for keys that have BOTH an ASCII code and a scancode
 * (Enter / Esc / printable letters). The press goes into the BIOS queue
 * with the ASCII+scancode pair (some game code reads ASCII via INT 16h),
 * while the matching release in mark_scancode_released uses the scancode
 * alone. Deduplicates against SDL key-repeat so a held key doesn't flood
 * the queue with makes. */
static void mark_ascii_pressed(uint8_t ascii) {
  if (!ascii) {
    return;
  }
  /* shim_keyboard_enqueue_press_ascii pushes (ascii, ascii_to_scan(ascii))
   * — we mirror its scancode mapping here so the release uses the same
   * key state slot. */
  uint8_t scancode = 0;
  switch (ascii) {
    case '\r': case '\n': scancode = 0x1C; break;
    case 27:              scancode = 0x01; break;
    case ' ':             scancode = 0x39; break;
    case 0x08:            scancode = 0x0E; break;
    case '\t':            scancode = 0x0F; break;
    default:
      if (ascii >= 'a' && ascii <= 'z') {
        static const uint8_t map[] = {
            0x1E, 0x30, 0x2E, 0x20, 0x12, 0x21, 0x22, 0x23, 0x17,
            0x24, 0x25, 0x26, 0x32, 0x31, 0x18, 0x19, 0x10, 0x13,
            0x1F, 0x14, 0x16, 0x2F, 0x11, 0x2D, 0x15, 0x2C};
        scancode = map[ascii - 'a'];
      } else if (ascii >= '0' && ascii <= '9') {
        static const uint8_t map[] = {0x0B, 0x02, 0x03, 0x04, 0x05,
                                      0x06, 0x07, 0x08, 0x09, 0x0A};
        scancode = map[ascii - '0'];
      }
      break;
  }
  if (!scancode) {
    return;
  }
  if (pressed_scancodes[scancode]) {
    return; /* already down — drop SDL key-repeat make */
  }
  pressed_scancodes[scancode] = true;
  shim_keyboard_enqueue(ascii);
}

static void sync_pressed_scancodes_with_keyboard_state(void) {
  // Refresh the snapshot before reading it so we see the latest physical
  // state even if SDL chose not to queue matching events (e.g. under heavy
  // load when polling lags behind OS input).
  SDL_PumpEvents();
  int num_keys = 0;
  const uint8_t *state = SDL_GetKeyboardState(&num_keys);
  for (size_t i = 0; i < sizeof(scancode_mappings) / sizeof(scancode_mappings[0]); ++i) {
    const SDL_Scancode sdl_scancode = scancode_mappings[i].sdl_scancode;
    const uint8_t dos_scancode = scancode_mappings[i].dos_scancode;
    const bool is_down = (sdl_scancode < num_keys) && state[sdl_scancode];
    if (is_down) {
      // Resend the press if the key state changed while we weren't polling
      // events (e.g. multiple simultaneous key presses). This keeps our
      // internal pressed_scancodes[] in sync with SDL's view of the world and
      // prevents keys from appearing "stuck" when a KEYUP event goes missing.
      mark_scancode_pressed(dos_scancode);
    } else {
      mark_scancode_released(dos_scancode);
    }
  }
}

static void handle_events(void) {
  SDL_PumpEvents();
  SDL_Event e;
  while (SDL_PollEvent(&e)) {
    switch (e.type) {
      case SDL_QUIT:
        /* Window close button, Alt+F4, Cmd+Q, etc. Log it so a silent
         * window-close doesn't look like an unexplained crash. */
        fprintf(stderr,
                "\n[EXIT] SDL_QUIT received (window closed / Alt+F4 / "
                "Cmd+Q) — clean exit(0). No crash bundle.\n");
        save_manager_sr_log("exit SDL_QUIT window_close clean_exit");
        fflush(stderr);
        SDL_Quit();
        exit(0);
        break;

      case SDL_WINDOWEVENT:
        switch (e.window.event) {
          case SDL_WINDOWEVENT_SIZE_CHANGED:
            // Nothing special: renderer has logical size set.
            break;
          case SDL_WINDOWEVENT_FOCUS_LOST:
          case SDL_WINDOWEVENT_MINIMIZED:
          case SDL_WINDOWEVENT_HIDDEN:
          case SDL_WINDOWEVENT_LEAVE:
            release_pressed_scancodes();
            break;
          default:
            break;
        }
        break;

      case SDL_KEYDOWN: {
        const SDL_Keymod mods = (SDL_Keymod)e.key.keysym.mod;

        if (e.key.keysym.sym == SDLK_LCTRL || e.key.keysym.sym == SDLK_RCTRL) {
          ctrl_pressed = true;
          break;
        }

        const bool ctrl_down = ctrl_pressed || (mods & KMOD_CTRL);

        // Ctrl+P -> save a screenshot under artifacts/screenshots.
        if (ctrl_down && e.key.keysym.scancode == SDL_SCANCODE_P) {
          if (!e.key.repeat) {
            shim_save_video_memory();
          }
          break;
        }

        // ⌘/Win + (1|2|3) -> pick visible buffer
        // ⌘/Win + F1 -> manual save (Cmd+F1)
        // ⌘/Win + F2 -> load latest save (Cmd+F2)
        if (mods & KMOD_GUI) {
          switch (e.key.keysym.sym) {
            case SDLK_1:
              virtual_display_buffer = 0;
              SDL_SetWindowTitle(window, "Saisei - Main Buffer");
              break;
            case SDLK_2:
              virtual_display_buffer = 1;
              SDL_SetWindowTitle(window, "Saisei - First Buffer");
              break;
            case SDLK_3:
              virtual_display_buffer = 2;
              SDL_SetWindowTitle(window, "Saisei - Second Buffer");
              break;
            case SDLK_F1:
              if (!e.key.repeat) save_manager_request_save();
              break;
            case SDLK_F2:
              if (!e.key.repeat) save_manager_request_load_latest();
              break;
            default: break;
          }
          break;
        }

        // Plain F9 / F10 = bookend recorder start/stop (debug aid for
        // symptom-only bugs). Cmd+F1/F2 above remain reserved for save/load.
        if (e.key.keysym.sym == SDLK_F9) {
          if (!e.key.repeat) shim_bookend_start();
          break;
        }
        if (e.key.keysym.sym == SDLK_F10) {
          if (!e.key.repeat) shim_bookend_stop();
          break;
        }

        // Simple keypad passthrough (DOS scancodes). Every keydown enqueues
        // a make scancode and the matching SDL_KEYUP below enqueues the
        // matching break, so the game's IRQ1 ISR sees a full keystroke
        // instead of seeing the key as held forever. mark_*_pressed
        // deduplicate against SDL key-repeat.
        switch (e.key.keysym.sym) {
          case SDLK_UP:        mark_scancode_pressed(0x48); break;
          case SDLK_DOWN:      mark_scancode_pressed(0x50); break;
          case SDLK_LEFT:      mark_scancode_pressed(0x4B); break;
          case SDLK_RIGHT:     mark_scancode_pressed(0x4D); break;
          case SDLK_RETURN:    mark_ascii_pressed('\r');     break;
          case SDLK_ESCAPE:    mark_ascii_pressed(27);       break;
          case SDLK_BACKSPACE: mark_ascii_pressed(0x08);     break;
          case SDLK_TAB:       mark_ascii_pressed('\t');     break;
          default: {
            const SDL_Keycode keycode = e.key.keysym.sym;
            if (keycode > 0 && keycode < 0x80) {
              mark_ascii_pressed((uint8_t)keycode);
            }
            break;
          }
        }
        break;
      }

      case SDL_KEYUP:
        if (e.key.keysym.sym == SDLK_LCTRL || e.key.keysym.sym == SDLK_RCTRL) {
          ctrl_pressed = false;
          break;
        }
        switch (e.key.keysym.sym) {
          case SDLK_UP:        mark_scancode_released(0x48); break;
          case SDLK_DOWN:      mark_scancode_released(0x50); break;
          case SDLK_LEFT:      mark_scancode_released(0x4B); break;
          case SDLK_RIGHT:     mark_scancode_released(0x4D); break;
          case SDLK_RETURN:    mark_scancode_released(0x1C); break;
          case SDLK_ESCAPE:    mark_scancode_released(0x01); break;
          case SDLK_BACKSPACE: mark_scancode_released(0x0E); break;
          case SDLK_TAB:       mark_scancode_released(0x0F); break;
          default: {
            const SDL_Keycode keycode = e.key.keysym.sym;
            if (keycode > 0 && keycode < 0x80) {
              /* Mirror the ascii→scancode mapping used by mark_ascii_pressed
               * so the release pairs with the right slot. */
              uint8_t scancode = 0;
              if (keycode >= 'a' && keycode <= 'z') {
                static const uint8_t map[] = {
                    0x1E, 0x30, 0x2E, 0x20, 0x12, 0x21, 0x22, 0x23, 0x17,
                    0x24, 0x25, 0x26, 0x32, 0x31, 0x18, 0x19, 0x10, 0x13,
                    0x1F, 0x14, 0x16, 0x2F, 0x11, 0x2D, 0x15, 0x2C};
                scancode = map[keycode - 'a'];
              } else if (keycode >= '0' && keycode <= '9') {
                static const uint8_t map[] = {0x0B, 0x02, 0x03, 0x04, 0x05,
                                              0x06, 0x07, 0x08, 0x09, 0x0A};
                scancode = map[keycode - '0'];
              } else if (keycode == ' ') {
                scancode = 0x39;
              }
              if (scancode) {
                mark_scancode_released(scancode);
              }
            }
            break;
          }
        }
        break;

      case SDL_MOUSEMOTION: {
        /* SDL_RenderSetLogicalSize makes SDL report event coordinates already
         * scaled into the LOGICAL framebuffer space (320x200; confirmed: an
         * event at the bottom-right corner reads ~316,196, not the window's
         * 960,600). Map them against the logical size -- mapping against the
         * window size scaled the driver position down by window/logical, so the
         * game cursor tracked at a fraction of the real cursor's speed and
         * drifted off toward the bottom-right. */
        mouse_host_motion(e.motion.x, e.motion.y, win_w, win_h);
        break;
      }

      case SDL_MOUSEBUTTONDOWN:
      case SDL_MOUSEBUTTONUP: {
        mouse_host_motion(e.button.x, e.button.y, win_w, win_h);
        int b = (e.button.button == SDL_BUTTON_RIGHT)    ? 1
              : (e.button.button == SDL_BUTTON_MIDDLE)   ? 2
                                                         : 0;
        mouse_host_button(b, e.type == SDL_MOUSEBUTTONDOWN);
        break;
      }

      default: break;
    }
  }
  sync_pressed_scancodes_with_keyboard_state();
}

void virtual_display_poll_input(void) {
  // SDL_Init may not have run yet; skip polling in that case.
  if (!SDL_WasInit(SDL_INIT_VIDEO) || !display_ready) {
    return;
  }
  static uint32_t last_poll_ticks;
  const uint32_t now = SDL_GetTicks();
  if (!SDL_TICKS_PASSED(now, last_poll_ticks + 4)) {
    return;
  }
  last_poll_ticks = now;

  /*
   * Drain SDL's event queue even when no frame is being presented. During
   * CPU-heavy periods, only syncing keyboard state is not enough because SDL's
   * internal event queue can overflow and drop KEYUP events, which leaves DOS
   * keys logically pressed.
   */
  handle_events();
  /* Periodic auto-save is disabled, so we don't call
   * save_manager_tick here. We still drive save_manager_poll_pending so
   * a Cmd+F1 request queued in handle_events gets serviced at the next
   * safe SAFEPOINT (this poll itself runs from one). Cmd+F2 re-execs
   * inline from the event handler, so it doesn't need the pending loop. */
  save_manager_poll_pending();
}

// ----- public API -----

void virtual_display_init(int width, int height, int scale) {
  display_ready = false;
  if (scale > 0) scale_hint = scale;
  win_w = width;
  win_h = height;

  /* Tell SDL not to install its own signal handlers. Without this SDL
   * overrides our crash_signal_handler (set in init_shim_logs constructor)
   * and SIGSEGV/SIGABRT go to SDL's silent path — the [CRASH] marker
   * never gets written and the game looks like it died for no reason. */
  SDL_SetHint("SDL_NO_SIGNAL_HANDLERS", "1");

  if (SDL_Init(SDL_INIT_VIDEO) != 0) {
    fprintf(stderr, "SDL_Init failed: %s\n", SDL_GetError());
    return;
  }

  /* Belt-and-suspenders: even with SDL_NO_SIGNAL_HANDLERS, some video
   * drivers (X11, mesa) install their own handlers via dlopen. Re-install
   * ours so SIGSEGV definitely lands in our handler. */
  extern void shim_reinstall_crash_handlers(void);
  shim_reinstall_crash_handlers();

  SDL_SetHint(SDL_HINT_RENDER_SCALE_QUALITY, "0"); // nearest neighbor
  window = SDL_CreateWindow(
      "Saisei",
      SDL_WINDOWPOS_UNDEFINED, SDL_WINDOWPOS_UNDEFINED,
      width * scale_hint, height * scale_hint,
      SDL_WINDOW_RESIZABLE);
  if (!window) {
    fprintf(stderr, "SDL_CreateWindow failed: %s\n", SDL_GetError());
    SDL_Quit();
    return;
  }

  renderer = SDL_CreateRenderer(window, -1, SDL_RENDERER_ACCELERATED);
  if (!renderer) {
    fprintf(stderr, "SDL_CreateRenderer failed: %s\n", SDL_GetError());
    SDL_DestroyWindow(window);
    SDL_Quit();
    return;
  }

  recreate_texture(width, height);
}

void virtual_display_shutdown(void) {
  free(rgb_buffer); rgb_buffer = NULL; rgb_buffer_size = 0;
  if (texture)  { SDL_DestroyTexture(texture);  texture = NULL; }
  if (renderer) { SDL_DestroyRenderer(renderer); renderer = NULL; }
  if (window)   { SDL_DestroyWindow(window);    window = NULL; }
  display_ready = false;
  texture_w = texture_h = 0;
  SDL_Quit();
}

void virtual_display_present(const uint8_t *vram, int pitch, int w, int h,
                             const uint8_t *palette, uint8_t palette_mask) {
  // Consume input first so hotkeys feel responsive.
  handle_events();
  if (!texture || !renderer) return;
  if (!vram || !palette)     return;

  ensure_rgb_buffer(w, h);

  // Convert indices → RGB24
  // IMPORTANT: mask applies to DAC components, **not** the index.
  for (int y = 0; y < h; ++y) {
    const uint8_t *src = vram + y * pitch;
    uint8_t *dst = rgb_buffer + y * w * 3;

    for (int x = 0; x < w; ++x) {
      const uint8_t idx = src[x];
      const uint8_t r6 = (uint8_t)(palette[idx * 3 + 0] & palette_mask);
      const uint8_t g6 = (uint8_t)(palette[idx * 3 + 1] & palette_mask);
      const uint8_t b6 = (uint8_t)(palette[idx * 3 + 2] & palette_mask);
      dst[x * 3 + 0] = vga6_to_8(r6);
      dst[x * 3 + 1] = vga6_to_8(g6);
      dst[x * 3 + 2] = vga6_to_8(b6);
    }
  }

  // Upload & present
  SDL_UpdateTexture(texture, NULL, rgb_buffer, w * 3);
  SDL_RenderClear(renderer);
  SDL_RenderCopy(renderer, texture, NULL, NULL);
  SDL_RenderPresent(renderer);
}

void virtual_display_set_mode(int mode) {
  // For now the engine drives 320x200 8bpp (MCGA / Mode 13h).
  // If you add other modes, rebuild the texture here based on 'mode'.
  (void)mode;
}

void virtual_display_configure(int width, int height) {
  win_w = width;
  win_h = height;
  recreate_texture(width, height);
}
