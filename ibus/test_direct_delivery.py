"""Direct delivery for clients without surrounding text: decide, deliver, never revise.

Rich web editors (the Gmail compose box is the reference case) re-render the DOM
during IME composition and turn every preedit state into permanent text,
including the grey ghost suffix.  Chrome and Electron reach IBus through GTK
im-ibus without SURROUNDING_TEXT, so nothing already delivered can be verified
there, and a forwarded BackSpace is queued while ``commit_text`` is applied at
once ("delete N, then insert" can run out of order).

For such clients the adapter never shows a preedit and never revises: while
the core's dual buffer is in its hypothesis phase nothing is delivered; once
the language locks the word is committed in its final script and later keys
only append.  The only deletion is one character for the operator's own
consumed BackSpace; any other revision is refused and the core's word ends.
Keys the client handles itself first receive the undelivered word, then the
word ends, so nothing typed later can revise text at a caret the engine cannot
see.

The editor model applies the engine's output in order: commits insert at the
caret, forwarded BackSpace deletes before it, keys the engine does not consume
are handled by the client.  Safety: fake IBus + scripted fake core (see
``test_space_accept``); the live engine, bus and native module are never
reachable.
"""

from __future__ import annotations

import pytest

from .test_space_accept import FakeCore, build_engine

CHROME_CAPS = 0x09   # PREEDIT_TEXT | FOCUS — Chrome/Electron via GTK im-ibus
NATIVE_CAPS = 0x29   # + SURROUNDING_TEXT — Konsole/Qt, ordinary GTK widgets
BACKSPACE = 0xFF08
RELEASE = 1 << 30
CONTROL = 1 << 2
SUPER = 1 << 26

KEYCODE = {"n": 49, "a": 30, "b": 48, "y": 21, "l": 38, "g": 34, "r": 19,
           "s": 31, "k": 37, "i": 23, "c": 46, "p": 25, "o": 24, "z": 44,
           "space": 57, "BackSpace": 14, "Tab": 15, "Escape": 1, "NumLock": 69}
KEYVAL = {"space": 0x20, "BackSpace": BACKSPACE, "Tab": 0xFF09, "Escape": 0xFF1B,
          "NumLock": 0xFF7F}

UNLOCKED = (True, False, True)   # (dual_buffer_present, locked, hypothesis_phase)
LOCKED = (True, True, False)
NO_WORD = (False, False, False)


class Core(FakeCore):
    """Scripted core; each press also scripts the lock state it reports."""

    def __init__(self, scripts, states=None, flush=None, cancel=None):
        super().__init__(scripts)
        self._states = list(states or [])
        self._state = NO_WORD
        self._flush = flush if flush is not None else []
        self._cancel = cancel if cancel is not None else []
        self.events = []
        self.keycodes = []
        self.external_rewrite = None

    def process_keycode(self, evdev, state):
        self.keycodes.append((evdev, state))
        return super().process_keycode(evdev, state)

    def handle_key(self, keyval, state):  # keyval path used by the fake harness
        self.keycodes.append((keyval, state))
        return super().handle_key(keyval, state)

    def _next(self):
        if self._states:
            self._state = self._states.pop(0)
        return super()._next()

    def debug_state(self):
        return self._state

    def focus_gained(self):
        self.events.append("focus_gained")

    def focus_lost(self):
        self.events.append("focus_lost")
        return list(self._flush)

    def reset(self):
        self.events.append("reset")
        return list(self._cancel)

    def set_surrounding_text(self, _text, _cursor):
        return None

    def save_personal(self):
        return None

    def set_external_rewrite_supported(self, supported):
        self.external_rewrite = supported


def build(steps=(), caps=CHROME_CAPS, **core_kw):
    scripts, states = [], []
    for _key, actions, state in steps:
        scripts += [actions, [("forward", "")]]      # press, then its release
        states += [state, state]
    eng, _rec = build_engine(scripts)
    eng._core = Core(scripts, states=states, **core_kw)
    eng._last_save = 0.0
    log = []
    eng.commit_text = lambda t: log.append(("commit", t.s))
    eng.update_preedit_text = lambda t, _c, visible: log.append(("preedit", t.s, visible))
    eng.forward_key_event = lambda kv, kc, st: log.append(("fwd", kv, kc, st))
    eng.delete_surrounding_text = lambda off, n: log.append(("delete_surrounding", off, n))
    eng.do_set_capabilities(caps)
    eng._core.events.clear()  # only what the test itself causes is asserted
    return eng, log


def press(eng, log, name, state=0):
    """Press + release one key; a key the engine does not consume is handled by
    the client itself."""
    keyval = KEYVAL[name] if name in KEYVAL else ord(name)
    keycode = KEYCODE[name]
    consumed = eng.do_process_key_event(keyval, keycode, state)
    if not consumed:
        log.append(("client", name, state))
    eng.do_process_key_event(keyval, keycode, state | RELEASE)
    return consumed


def editor(log, initial=""):
    """Editor with a caret.  ('client', name, state) is a key the client handled;
    ('move', pos) is a caret move the engine never hears about."""
    text, pos = list(initial), len(initial)
    for event in log:
        kind = event[0]
        if kind == "commit":
            for ch in event[1]:
                text.insert(pos, ch)
                pos += 1
        elif kind == "fwd" and event[1] == BACKSPACE and not event[3] & RELEASE:
            if pos:
                pos -= 1
                del text[pos]
        elif kind == "client":
            name, state = event[1], event[2]
            if name == "BackSpace" and state & CONTROL:  # delete the previous word
                while pos and text[pos - 1] != " ":
                    pos -= 1
                    del text[pos]
            elif name == "BackSpace" and pos:
                pos -= 1
                del text[pos]
            elif name == "space":
                text.insert(pos, " ")
                pos += 1
            elif len(name) == 1 and not state & CONTROL:  # a letter typed natively
                text.insert(pos, name)
                pos += 1
        elif kind == "move":
            pos = event[1]
    return "".join(text)


def backspaces(log):
    return [e for e in log if e[0] == "fwd" and e[1] == BACKSPACE and not e[3] & RELEASE]


def commits(log):
    return [e[1] for e in log if e[0] == "commit"]


def visible_preedits(log):
    return [e for e in log if e[0] == "preedit" and e[1]]


# Synthetic replay of a short Bulgarian phrase typed phonetically while the
# keyboard layout is US ("po byrzina "): the core's actions and lock state
# per press, with the Latin hypothesis "by" before the language locks.
PHRASE = [
    ("p", [("composing", "п\x00о")], UNLOCKED),
    ("o", [("composing", "по\x00")], UNLOCKED),
    ("space", [("hide", ""), ("commit", "по"), ("forward", "")], NO_WORD),
    ("b", [("composing", "б\x00")], UNLOCKED),
    ("y", [("composing", "by\x00")], UNLOCKED),
    ("r", [("composing", "бър\x00")], LOCKED),
    ("z", [("composing", "бърз\x00")], LOCKED),
    ("i", [("composing", "бързи\x00на")], LOCKED),
    ("n", [("composing", "бързин\x00а")], LOCKED),
    ("a", [("composing", "бързина\x00")], LOCKED),
    ("space", [("hide", ""), ("commit", "бързина"), ("forward", "")], NO_WORD),
]


def run(steps, **kw):
    eng, log = build(steps, **kw)
    for key, _actions, _state in steps:
        press(eng, log, key)
    return eng, log


# --- the reported bug ---------------------------------------------------------

def test_phrase_is_typed_exactly_without_preedit_or_deletion():
    _eng, log = run(PHRASE)
    assert editor(log) == "по бързина "
    assert visible_preedits(log) == [], "direct delivery must never use the preedit"
    assert backspaces(log) == [], "hypothesis-phase flips must never need a deletion"


def test_the_ghost_suffix_is_never_typed():
    _eng, log = run(PHRASE)
    assert editor(log).count("на") == 1


def test_nothing_is_delivered_before_the_language_locks():
    _eng, log = run(PHRASE[3:5])  # "б", "by" — still a hypothesis
    assert editor(log) == ""
    _eng, log = run(PHRASE[3:6])  # locks at "бър"
    assert editor(log) == "бър"
    assert backspaces(log) == []


def test_locked_word_only_appends():
    _eng, log = run(PHRASE[3:9])
    assert commits(log) == ["бър", "з", "и", "н"]


# --- delivered text is never revised -------------------------------------------

def test_a_revision_after_delivery_is_refused_and_the_word_ends():
    steps = [
        ("b", [("composing", "бъ\x00")], LOCKED),
        ("y", [("composing", "by\x00")], LOCKED),
    ]
    eng, log = build(steps)
    eng._caps = 0x29  # even a client claiming SURROUNDING_TEXT gets no rewrite here
    press(eng, log, "b")
    assert press(eng, log, "y") is False, "the refused key goes to the client"
    assert editor(log) == "бъy", "delivered text stays; the letter is typed, not lost"
    assert backspaces(log) == []
    assert not [e for e in log if e[0] == "delete_surrounding"]
    assert "reset" in eng._core.events, "the core's word must end"


def test_backspace_in_the_hypothesis_phase_outputs_nothing():
    steps = [
        ("b", [("composing", "б\x00")], UNLOCKED),
        ("y", [("composing", "бъ\x00")], UNLOCKED),
        ("BackSpace", [("composing", "б\x00")], UNLOCKED),
        ("BackSpace", [("hide", "")], NO_WORD),
    ]
    _eng, log = run(steps)
    assert commits(log) == [] and backspaces(log) == []


def test_operator_backspace_after_delivery_deletes_one_character_each():
    steps = PHRASE[3:7] + [
        ("BackSpace", [("composing", "бър\x00")], LOCKED),
        ("BackSpace", [("composing", "бъ\x00")], LOCKED),
        ("BackSpace", [("composing", "б\x00")], LOCKED),
        ("BackSpace", [("hide", "")], NO_WORD),
    ]
    _eng, log = run(steps)
    assert editor(log) == ""
    assert len(backspaces(log)) == 4
    presses = [e for e in log if e[0] == "fwd" and not e[3] & RELEASE]
    releases = [e for e in log if e[0] == "fwd" and e[3] & RELEASE]
    assert len(presses) == len(releases), "each forwarded BackSpace needs its release"


def test_a_multi_character_deletion_is_refused_even_on_backspace():
    steps = PHRASE[3:7] + [("BackSpace", [("composing", "б\x00")], LOCKED)]
    eng, log = run(steps)
    assert editor(log) == "бър", "one BackSpace deletes exactly one character"
    assert backspaces(log) == [], "the deletion is the client's own"
    assert "reset" in eng._core.events


# --- keys the client handles ------------------------------------------------------

def test_a_forwarded_key_first_receives_the_word_and_then_the_word_ends():
    steps = PHRASE[3:5] + [("c", [("forward", "")], UNLOCKED)]  # e.g. Ctrl+C
    eng, log = build(steps)
    press(eng, log, "b")
    press(eng, log, "y")
    assert editor(log) == ""
    press(eng, log, "c", state=CONTROL)
    commit_at = [i for i, e in enumerate(log) if e[0] == "commit"]
    key_at = [i for i, e in enumerate(log) if e[:2] == ("client", "c")]
    assert commit_at and key_at and commit_at[-1] < key_at[0]
    assert commits(log) == ["by"]
    assert "reset" in eng._core.events, "the word ends before the client acts"


def test_ctrl_backspace_then_a_flip_never_deletes_text_before_the_word():
    # Audit repro: the core keeps its word across a Ctrl chord and later flips.
    steps = [
        ("b", [("composing", "б\x00")], UNLOCKED),
        ("y", [("composing", "by\x00")], UNLOCKED),
        ("BackSpace", [("forward", "")], UNLOCKED),
        ("n", [("composing", "бън\x00")], LOCKED),
    ]
    eng, log = build(steps)
    press(eng, log, "b")
    press(eng, log, "y")
    press(eng, log, "BackSpace", state=CONTROL)  # client deletes the word "by"
    press(eng, log, "n")
    assert editor(log, initial="по ") == "по бън"
    assert backspaces(log) == []


def test_a_click_reset_discards_the_hypothesis_instead_of_moving_it():
    # Chrome sends Reset on a mouse press; an undelivered hypothesis is then
    # dropped like a cleared preedit, never typed at the new caret.
    steps = [
        ("b", [("composing", "б\x00")], UNLOCKED),
        ("y", [("composing", "by\x00")], UNLOCKED),
        ("l", [("composing", "л\x00")], UNLOCKED),
    ]
    eng, log = build(steps, cancel=[("hide", "")])
    press(eng, log, "b")
    press(eng, log, "y")
    log.append(("move", 0))
    eng.do_reset()
    press(eng, log, "l")
    assert editor(log, initial="hello ") == "hello "


def test_releases_do_not_flush_the_hypothesis():
    _eng, log = run(PHRASE[3:5])
    assert commits(log) == []


def test_inert_keys_do_not_flush_or_end_the_word():
    steps = PHRASE[3:5] + [("NumLock", [("forward", "")], UNLOCKED)]
    eng, log = build(steps)
    for key, _a, _s in steps:
        press(eng, log, key)
    assert commits(log) == []
    assert "reset" not in eng._core.events


def test_tab_types_the_word_but_never_an_undisplayed_ghost():
    steps = [("b", [("composing", "б\x00ързина")], UNLOCKED)]
    eng, log = build(steps, cancel=[("hide", "")])
    press(eng, log, "b")
    assert press(eng, log, "Tab") is False, "Tab belongs to the client"
    assert editor(log) == "б"
    assert "reset" in eng._core.events


def test_super_escape_kill_switch_still_reaches_the_core():
    eng, _log = build()
    eng.do_process_key_event(KEYVAL["Escape"], KEYCODE["Escape"], SUPER)
    assert eng._core.keycodes, "Super+Escape must reach the core's kill switch"


# --- sensitive fields ---------------------------------------------------------------

def test_sensitivity_switch_forgets_the_word_without_deleting_it():
    steps = PHRASE[3:7] + [("n", [("composing", "н\x00")], UNLOCKED),
                           ("space", [("commit", "н"), ("forward", "")], NO_WORD)]
    eng, log = build(steps, cancel=[("hide", "")])
    for key, _a, _s in steps[:4]:
        press(eng, log, key)
    assert editor(log) == "бърз"
    eng._switch_sensitivity(True)
    eng._switch_sensitivity(False)
    assert eng._direct_word == "" and eng._direct_pending == ""
    press(eng, log, "n")
    press(eng, log, "space")
    assert editor(log) == "бързн "
    assert backspaces(log) == []


def test_tab_in_a_sensitive_field_never_replays_the_discarded_word():
    eng, log = build(PHRASE[3:5], cancel=[("hide", "")])
    press(eng, log, "b")
    press(eng, log, "y")
    eng._switch_sensitivity(True)
    eng.do_process_key_event(KEYVAL["Tab"], KEYCODE["Tab"], 0)
    assert commits(log) == []


# --- focus and cancel paths -------------------------------------------------------------

def test_focus_out_keeps_delivered_text_and_flushes_nothing():
    eng, log = build(PHRASE[3:8], flush=[("commit", "бързи"), ("hide", "")])
    for key, _a, _s in PHRASE[3:8]:
        press(eng, log, key)
    eng.do_focus_out()
    assert editor(log) == "бързи"
    assert "focus_lost" not in eng._core.events, "no flush may race into the next window"
    assert "reset" in eng._core.events


def test_focus_out_drops_an_undelivered_hypothesis():
    # By the time focus-out runs, ibus-daemon has already moved its focus: a
    # flushed hypothesis could land in the next window.  It is dropped instead,
    # like a cleared preedit.
    eng, log = build(PHRASE[3:5], flush=[("commit", "by"), ("hide", "")])
    for key, _a, _s in PHRASE[3:5]:
        press(eng, log, key)
    eng.do_focus_out()
    assert commits(log) == []


# --- refused revisions never swallow the operator's key ----------------------------

def test_a_refused_backspace_is_handled_by_the_client():
    # Re-audit probe A: after "бър" was delivered the core unlocks on BackSpace
    # and re-scores to the Latin "by"; the revision is refused, so the client
    # deletes one character natively instead of the key doing nothing.
    steps = PHRASE[3:6] + [("BackSpace", [("composing", "by\x00")], UNLOCKED)]
    eng, log = build(steps)
    for key, _a, _s in steps[:-1]:
        press(eng, log, key)
    assert press(eng, log, "BackSpace") is False
    assert editor(log) == "бъ"
    assert backspaces(log) == [], "the deletion is the client's own"
    assert "reset" in eng._core.events


def test_a_refused_letter_is_typed_by_the_client_not_lost():
    # Re-audit probe B: BackSpace to "бъ", then the next letter flips to "byr".
    steps = PHRASE[3:6] + [
        ("BackSpace", [("composing", "бъ\x00")], UNLOCKED),
        ("r", [("composing", "byr\x00")], UNLOCKED),
    ]
    eng, log = build(steps)
    for key, _a, _s in steps[:-1]:
        press(eng, log, key)
    assert press(eng, log, "r") is False
    assert editor(log) == "бъr", "the letter must not be lost"


def test_one_operator_backspace_never_deletes_two_characters():
    steps = PHRASE[3:6] + [
        ("BackSpace", [("composing", "бъ\x00"), ("composing", "б\x00")], LOCKED),
    ]
    eng, log = build(steps)
    for key, _a, _s in steps:
        press(eng, log, key)
    assert editor(log) == "бъ"
    assert len(backspaces(log)) == 1


def test_no_action_runs_after_a_refusal_in_the_same_batch():
    steps = [
        ("b", [("composing", "бъ\x00")], LOCKED),
        ("y", [("composing", "by\x00"), ("commit", "by")], LOCKED),
    ]
    _eng, log = run(steps)
    assert commits(log) == ["бъ"], "a refused word must never be delivered again"


# --- delivery-mode transitions -----------------------------------------------------------

def test_switching_to_direct_takes_down_a_live_preedit_and_ends_the_word():
    steps = [("b", [("composing", "б\x00ързина")], UNLOCKED)]
    eng, log = build(steps, caps=NATIVE_CAPS)
    press(eng, log, "b")
    assert visible_preedits(log), "preedit mode showed the composing word"
    eng.do_set_capabilities(CHROME_CAPS)
    assert eng._direct_delivery is True
    last_preedit = [e for e in log if e[0] == "preedit"][-1]
    assert last_preedit == ("preedit", "", False), "the preedit (and its ghost) is taken down"
    assert "reset" in eng._core.events
    assert commits(log) == []


def test_switching_back_to_preedit_ends_a_delivered_word():
    eng, log = build(PHRASE[3:6])
    for key, _a, _s in PHRASE[3:6]:
        press(eng, log, key)
    assert editor(log) == "бър"
    eng._core.events.clear()
    eng.do_set_capabilities(NATIVE_CAPS)
    assert eng._direct_delivery is False
    assert "reset" in eng._core.events
    assert eng._direct_word == "" and eng._direct_pending == ""


def test_reset_keeps_delivered_text_and_starts_a_new_word():
    steps = PHRASE[3:8] + [("n", [("composing", "н\x00")], UNLOCKED),
                           ("space", [("commit", "н"), ("forward", "")], NO_WORD)]
    eng, log = build(steps, cancel=[("hide", "")])
    for key, _a, _s in steps[:5]:
        press(eng, log, key)
    eng.do_reset()
    press(eng, log, "n")
    press(eng, log, "space")
    assert editor(log) == "бързин "


# --- mode selection ---------------------------------------------------------------------

@pytest.mark.parametrize(
    ("caps", "mode", "direct"),
    [
        (CHROME_CAPS, "auto", True),
        (0x01, "auto", True),
        (NATIVE_CAPS, "auto", False),
        (0x2D, "auto", False),
        (CHROME_CAPS, "off", False),
        (NATIVE_CAPS, "direct", True),
    ],
)
def test_delivery_mode_follows_capabilities_and_config(caps, mode, direct):
    eng, _log = build(caps=NATIVE_CAPS)
    eng._delivery_mode = mode
    eng.do_set_capabilities(caps)
    assert eng._direct_delivery is direct
    expected_rewrite = bool(caps & 0x20) and not direct
    assert eng._core.external_rewrite is expected_rewrite


def test_unknown_capabilities_keep_the_preedit():
    eng, _rec = build_engine([])
    assert getattr(eng, "_direct_delivery", False) is False
    eng._update_delivery_mode()
    assert eng._direct_delivery is False


def test_language_choice_is_unavailable_in_direct_delivery():
    class ChoiceCore(Core):
        def set_language_choice_supported(self, supported):
            self.choice = supported

        def language_choice_offer(self):
            return None

        def dismiss_language_choice(self, _revision):
            return True

        def accept_language_choice(self, *_args):
            return []

    for caps, expected in ((NATIVE_CAPS, True), (CHROME_CAPS, False)):
        eng, _log = build(caps=caps)
        eng._core = ChoiceCore([])
        eng._language_choice_route_available = True
        eng._caps = caps
        assert bool(eng._update_language_choice_support()) is expected


def test_other_clients_keep_the_composing_preedit():
    _eng, log = run(PHRASE, caps=NATIVE_CAPS)
    assert visible_preedits(log), "native clients keep today's composing preedit"
    assert commits(log) == ["по", "бързина"]
