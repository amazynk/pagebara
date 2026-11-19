"""An AT-SPI client that checks pagebara the way a screen reader sees it.

Run by run.sh on a private display and bus with the sample `tagged.pdf` open:
  check.py --enable     tells toolkits an assistive technology is listening
  check.py --pid PID    checks the app with that process id
Set A11Y_DUMP=1 to print the tree at the end.
"""

import os
import shutil
import subprocess
import sys
import time

import gi

gi.require_version("Atspi", "2.0")
from gi.repository import Atspi, Gio, GLib  # noqa: E402

S = Atspi.StateType
R = Atspi.Role

# Roles a user operates, which therefore need a name.
INTERACTIVE = {
    R.PUSH_BUTTON, R.TOGGLE_BUTTON, R.CHECK_BOX, R.RADIO_BUTTON, R.PAGE_TAB, R.ENTRY,
    R.PASSWORD_TEXT, R.COMBO_BOX, R.SLIDER, R.SPIN_BUTTON, R.LIST_ITEM, R.MENU_ITEM, R.LINK,
}
# Containers a screen reader announces when entering them.
CONTAINERS = {R.PAGE_TAB_LIST, R.LIST_BOX, R.LIST, R.LANDMARK, R.GROUPING}
# The tagged sample's structure in tag order (not drawing or layout order), its link read
# where it is, once.
READING_ORDER = [
    "Accessible sample",
    "First paragraph comes first in tag order.",
    "Second paragraph is drawn first in the content stream.",
    "• Apples",
    "• Pears",
    "A blue square",
    "Go to the second page",
    "Sidebar note: tagged last, drawn at the top.",
]

failures = []


def check(ok, what):
    print(("ok    " if ok else "FAIL  ") + what, flush=True)
    if not ok:
        failures.append(what)
    return ok


def enable():
    """Sets org.a11y.Status.IsEnabled once the bus launcher is up."""
    err = None
    for _ in range(100):
        try:
            c = Gio.bus_get_sync(Gio.BusType.SESSION, None)
            c.call_sync(
                "org.a11y.Bus", "/org/a11y/bus", "org.freedesktop.DBus.Properties", "Set",
                GLib.Variant("(ssv)", ("org.a11y.Status", "IsEnabled", GLib.Variant("b", True))),
                None, Gio.DBusCallFlags.NO_AUTO_START, 2000, None)
            return 0
        except GLib.Error as e:
            err = e
            time.sleep(0.1)
    print(err)
    return 1


# Events ------------------------------------------------------------------------------------

events = []


def on_event(e):
    try:
        src = e.source
        events.append((e.type, e.detail1, src.get_role(), src.get_name() or "", e.any_data))
    except GLib.Error:
        pass


def pump(secs):
    """Runs the main loop for `secs`, so events arrive."""
    ctx = GLib.MainContext.default()
    end = time.monotonic() + secs
    while time.monotonic() < end:
        while ctx.pending():
            ctx.iteration(False)
        time.sleep(0.01)


def wait_for(pred, secs=5.0):
    end = time.monotonic() + secs
    while time.monotonic() < end:
        pump(0.05)
        try:
            v = pred()
        except GLib.Error:
            v = None
        if v:
            return v
    return None


def take_events(kind):
    out = [e for e in events if e[0].startswith(kind)]
    events[:] = [e for e in events if not e[0].startswith(kind)]
    return out


def focused():
    """The objects that took the focus since last asked: (name, role name)."""
    return [(e[3], Atspi.role_get_name(e[2]))
            for e in take_events("object:state-changed:focused") if e[1] == 1]


# The tree ----------------------------------------------------------------------------------

class Node:
    def __init__(self, acc, path):
        self.acc = acc
        self.path = path
        self.role = acc.get_role()
        self.role_name = acc.get_role_name()
        self.name = acc.get_name() or ""
        self.description = acc.get_description() or ""
        self.states = acc.get_state_set()

    def has(self, state):
        return self.states.contains(state)

    def __repr__(self):
        return f"{self.role_name} {self.name!r} ({self.path})"

    def actions(self):
        if self.acc.get_action_iface() is None:
            return []
        n = Atspi.Action.get_n_actions(self.acc)
        return [Atspi.Action.get_action_name(self.acc, i) for i in range(n)]

    def act(self, i=0):
        return Atspi.Action.do_action(self.acc, i)

    def text(self):
        if self.acc.get_text_iface() is None:
            return None
        return Atspi.Text.get_text(self.acc, 0, Atspi.Text.get_character_count(self.acc))


def walk(acc, path="", out=None):
    out = [] if out is None else out
    acc.clear_cache()
    try:
        n = Node(acc, path)
        count = acc.get_child_count()
    except GLib.Error:
        return out
    out.append(n)
    for i in range(count):
        c = acc.get_child_at_index(i)
        if c is not None:
            walk(c, f"{path}/{n.name or n.role_name}", out)
    return out


def find_app(pid):
    desk = Atspi.get_desktop(0)
    for i in range(desk.get_child_count()):
        a = desk.get_child_at_index(i)
        try:
            if a is not None and a.get_process_id() == pid and a.get_child_count() > 0:
                return a
        except GLib.Error:
            pass
    return None


class App:
    def __init__(self, acc):
        self.acc = acc

    def nodes(self):
        return walk(self.acc)

    def find(self, role=None, name=None, within=None):
        for n in self.nodes():
            if role is not None and n.role != role:
                continue
            if within is not None and f"/{within}" not in n.path:
                continue
            if name is not None and n.name != name:
                continue
            return n
        return None

    def get(self, role, name, **kw):
        return wait_for(lambda: self.find(role, name, **kw))

    def under(self, landmark):
        return [n for n in self.nodes() if f"/{landmark}" in n.path]


# Keys go through the X server as a user's would (xdotool, XTEST), to the app's window,
# which gets the input focus first: there is no window manager to give it.
XDOTOOL = shutil.which("xdotool")
window = None


def xdotool(*args):
    return subprocess.run([XDOTOOL, *args], capture_output=True, text=True, timeout=20).stdout


def focus_window(pid):
    global window
    if window is None:
        ids = xdotool("search", "--sync", "--onlyvisible", "--pid", str(pid)).split()
        window = ids[0] if ids else None
    if window:
        xdotool("windowfocus", "--sync", window)


def key(name):
    """Presses a key, like "Tab" or "shift+Tab" (X keysym names)."""
    xdotool("key", "--clearmodifiers", name)
    pump(0.2)


def type_text(text):
    xdotool("type", "--delay", "30", text)
    pump(0.2)


# Checks ------------------------------------------------------------------------------------

def check_names(app, where):
    for n in app.nodes():
        if not n.has(S.SHOWING):
            continue
        if n.role in INTERACTIVE or n.role in CONTAINERS:
            check(n.name.strip() != "", f"{where}: {n} has a name")
        # Decoration (icons, empty text) is hidden from screen readers; the live region that
        # announces search results is there, empty, before the first result.
        live = n.path.endswith("/Find in document")
        if n.role in (R.IMAGE, R.LABEL) and n.name.strip() == "" and not live:
            check(False, f"{where}: {n} is decoration exposed to screen readers")


def check_structure(app):
    frame = app.find(R.FRAME)
    check(frame is not None and frame.name.startswith("tagged.pdf"),
          f"the window is named after the document: {frame}")
    for lm in ["Toolbar", "Tools", "Sidebar", "Document pages"]:
        check(app.find(R.LANDMARK, lm) is not None, f"landmark {lm}")
    # One document: its tab bar shows only with a second one.
    for name in ["Mode", "Sidebar view"]:
        check(app.find(R.PAGE_TAB_LIST, name) is not None, f"tab list {name}")
    check(app.find(R.LIST_BOX, "Pages") is not None, "the thumbnails are a list named Pages")
    check_names(app, "document open")


def check_reading_order(app):
    """The page's text as a screen reader reads it: tag order, roles in the description."""
    def page_text():
        items = [n for n in app.under("Document pages")
                 if n.role in (R.LABEL, R.LINK, R.PUSH_BUTTON) and "/Page 1" not in n.path]
        return items if len(items) >= len(READING_ORDER) else None

    items = wait_for(page_text, 8) or []
    names = [n.name for n in items]
    check(names == READING_ORDER, f"page text in tag order, links once: {names}")
    by_name = {n.name: n for n in items}
    heading = by_name.get("Accessible sample")
    check(heading is not None and heading.description == "Heading level 1, language en-GB",
          f"the heading says its level and language: {heading and heading.description!r}")
    item = by_name.get("• Apples")
    check(item is not None and item.description.startswith("List item"), "list items say so")
    figure = by_name.get("A blue square")
    check(figure is not None and figure.description.startswith("Figure"),
          f"the figure reads its alternate text: {figure and figure.description!r}")
    # Slint has no link role: links are buttons that say where they go.
    link = by_name.get("Go to the second page")
    check(link is not None and link.role == R.PUSH_BUTTON and link.description == "Link to page 2",
          f"the link says where it goes: {link} {link and link.description!r}")
    # Each item is where it is on the page: the figure is below the heading.
    if heading and figure:
        h = heading.acc.get_extents(Atspi.CoordType.WINDOW)
        f = figure.acc.get_extents(Atspi.CoordType.WINDOW)
        check(f.y > h.y and f.width > 10 and f.height > 10,
              f"the figure covers the image: {f.x},{f.y} {f.width}x{f.height}")
    return link


def page_number(app):
    e = app.find(R.ENTRY, "Page number")
    return e.text() if e else None


def check_states_and_actions(app, link):
    # Toggle buttons say "pressed" when on (AT-SPI's state for them).
    sidebar = app.get(R.TOGGLE_BUTTON, "Show sidebar")
    check(sidebar is not None and sidebar.has(S.PRESSED), "the sidebar toggle is pressed")
    check(sidebar is not None and sidebar.has(S.FOCUSABLE), "and focusable")
    sel = app.find(R.PAGE_TAB, "Select")
    check(sel is not None and sel.has(S.SELECTED), "the Select tool is the selected tab")
    hl = app.find(R.PAGE_TAB, "Highlight")
    check(hl is not None and not hl.has(S.SELECTED), "Highlight is not selected")
    pages_tab = app.find(R.PAGE_TAB, "Pages")
    check(pages_tab is not None and pages_tab.has(S.SELECTED), "the Pages sidebar tab is selected")
    page1 = app.find(R.LIST_ITEM, "Page 1")
    check(page1 is not None and page1.has(S.SELECTED), "the thumbnail of page 1 is selected")
    check(page_number(app) == "1", f"the page number field reads 1: {page_number(app)!r}")

    # Buttons invoked through AT-SPI do what a click does.
    def zoom_level():
        return next((n.name for n in app.nodes() if n.role == R.PUSH_BUTTON
                     and n.name.startswith("Zoom ") and n.name.endswith("%")), None)

    zin = app.find(R.PUSH_BUTTON, "Zoom in")
    check(zin is not None and len(zin.actions()) > 0, f"Zoom in has an action: {zin and zin.actions()}")
    before = zoom_level()
    if zin:
        zin.act()
    after = wait_for(lambda: (z := zoom_level()) != before and z)
    check(after is not None, f"Zoom in through AT-SPI zooms: {before} -> {after}")
    app.find(R.PUSH_BUTTON, "Zoom out").act()
    check(wait_for(lambda: zoom_level() not in (None, after)), f"and Zoom out zooms out: {zoom_level()}")

    if hl:
        hl.act()
    check(wait_for(lambda: (n := app.find(R.PAGE_TAB, "Highlight")) and n.has(S.SELECTED)),
          "choosing Highlight through AT-SPI selects it")
    check(wait_for(lambda: (n := app.find(R.PAGE_TAB, "Select")) and not n.has(S.SELECTED)),
          "and Select is then not selected")
    check(any(e[0] == "object:state-changed:selected" and e[3] == "Highlight" for e in events),
          "selection changes are events")
    app.find(R.PAGE_TAB, "Select").act()
    wait_for(lambda: (n := app.find(R.PAGE_TAB, "Select")) and n.has(S.SELECTED))

    take_events("object:state-changed")
    sidebar.act()
    check(wait_for(lambda: app.find(R.LANDMARK, "Sidebar") is None), "the sidebar toggle hides it")
    t = app.find(R.TOGGLE_BUTTON, "Show sidebar")
    check(t is not None and not t.has(S.PRESSED), "the toggle is then not pressed")
    t.act()
    check(wait_for(lambda: app.find(R.LANDMARK, "Sidebar") is not None), "and shows it again")
    check(wait_for(lambda: any(e[0] == "object:state-changed:pressed" and e[3] == "Show sidebar"
                               for e in events)), "pressed changes are events")

    # Following a link with the screen reader's default action.
    if link:
        check(len(link.actions()) > 0, f"the link has an action: {link.actions()}")
        link.act()
        check(wait_for(lambda: page_number(app) == "2"), "following the link goes to page 2")
        check(wait_for(lambda: (n := app.find(R.LIST_ITEM, "Page 2")) and n.has(S.SELECTED)),
              "and the thumbnail of page 2 is then selected")
        app.find(R.PUSH_BUTTON, "Back").act()
        check(wait_for(lambda: page_number(app) == "1"), "Back returns to page 1")


def check_focus(app, pid):
    focus_window(pid)
    check(wait_for(lambda: (f := app.find(R.FRAME)) and f.has(S.ACTIVE)),
          "the window is active once it has the input focus")
    focused()
    seen = []
    for _ in range(8):
        key("Tab")
        seen.extend(wait_for(focused, 2) or [])
    check(len(seen) >= 8, f"each Tab moves the focus, with an event: {seen}")
    for name, role in seen:
        check(name.strip() != "", f"the focused {role} has a name")
        # Focus on something the tree does not have reads as the window's title.
        check(role != "frame", f"the focus lands on a control, not the window: {name!r}")
    check(len(set(seen)) >= 6, "Tab reaches different controls")
    key("shift+Tab")
    back = wait_for(focused, 2) or []
    check(len(seen) >= 2 and back[-1:] == seen[-2:-1],
          f"Shift+Tab goes back: {back[-1:]} after {seen[-2:]}")
    # F6 moves between areas; the focus lands in each.
    areas = []
    for _ in range(5):
        key("F6")
        got = wait_for(focused, 2) or []
        areas.extend(got)
        check(len(got) > 0, f"F6 moves the focus to another area: {got}")
        if got:
            name, role = got[-1]
            n = next((n for n in app.nodes() if n.name == name and n.role_name == role
                      and n.has(S.FOCUSED)), None)
            check(n is not None, f"{role} {name!r} reports the focused state")
    check(any(n.startswith("Page 1 of 2") for n, _ in areas), f"the pages area says the page: {areas}")


def check_find_and_announcements(app):
    search = app.get(R.TOGGLE_BUTTON, "Search")
    check(search is not None and not search.has(S.PRESSED), "Search is a toggle, off")
    focused()
    search.act()
    field = app.get(R.ENTRY, "Find", within="Find in document")
    check(field is not None, f"the find field is named: {field}")
    got = wait_for(focused, 3) or []
    check(("Find", "entry") in got, f"the find field takes the focus: {got}")
    check(wait_for(lambda: (n := app.find(R.TOGGLE_BUTTON, "Search")) and n.has(S.PRESSED)),
          "Search is then pressed")
    take_events("object:announcement")
    type_text("paragraph")
    key("Return")
    ann = wait_for(lambda: take_events("object:announcement"), 4) or []
    said = [str(e[4]) for e in ann]
    check(any(" of " in a for a in said), f"the search result is announced: {said}")
    check(wait_for(lambda: (f := app.find(R.ENTRY, "Find")) and f.text() == "paragraph"),
          "the field's text is exposed")
    key("Escape")
    check(wait_for(lambda: app.find(R.ENTRY, "Find") is None), "Escape closes the find bar")


def check_read_aloud(app):
    ra = app.get(R.TOGGLE_BUTTON, "Read aloud")
    check(ra is not None and ra.has(S.SENSITIVE), f"Read aloud is available: {ra and ra.description!r}")
    take_events("object:announcement")
    ra.act()
    check(app.get(R.PANEL, "Read aloud") is not None or app.get(R.GROUPING, "Read aloud") is not None,
          "the read-aloud bar is shown")
    play = app.get(R.PUSH_BUTTON, "Read aloud from here")
    check(play is not None, f"it has a play button: {play}")
    if play:
        play.act()
    check(wait_for(lambda: app.find(R.PUSH_BUTTON, "Pause reading")),
          "while reading, the button pauses")
    ann = wait_for(lambda: [e for e in take_events("object:announcement")
                            if "Finished reading" in str(e[4])], 20)
    check(bool(ann), "the end of reading is announced")
    check(wait_for(lambda: app.find(R.PUSH_BUTTON, "Read aloud from here")),
          "then the button reads again")
    close = app.find(R.PUSH_BUTTON, "Close read aloud")
    check(close is not None, "the bar has a close button")
    if close:
        close.act()
        check(wait_for(lambda: app.find(R.PUSH_BUTTON, "Close read aloud") is None), "which closes it")


def check_menu(app):
    """The More menu: a popup whose rows are buttons that say their shortcuts."""
    more = app.get(R.PUSH_BUTTON, "More")
    focused()
    more.act()
    rows = wait_for(lambda: [n for n in app.nodes()
                             if "/More/" in n.path and n.role == R.PUSH_BUTTON]) or []
    names = [n.name for n in rows]
    check("Print…" in names and "Keyboard shortcuts" in names, f"More opens the menu: {names[:4]}")
    p = next((n for n in rows if n.name == "Print…"), None)
    check(p is not None and p.description == "Shortcut Ctrl+P", "menu rows say their shortcut")
    got = wait_for(focused, 2) or []
    check(("Open…", "button") in got, f"the first row takes the focus: {got}")
    check_names(app, "menu open")
    key("Escape")
    check(wait_for(lambda: not any("/More/" in n.path for n in app.nodes())), "Escape closes it")


def main():
    pid = int(sys.argv[sys.argv.index("--pid") + 1])
    listeners = []
    for kind in ["object:state-changed:focused", "object:state-changed:pressed",
                 "object:state-changed:selected", "object:announcement"]:
        listener = Atspi.EventListener.new(on_event)
        listener.register(kind)
        listeners.append(listener)
    acc = wait_for(lambda: find_app(pid), 30)
    if acc is None:
        print("FAIL  the app did not show up on the accessibility bus")
        return 1
    app = App(acc)
    if not app.get(R.LANDMARK, "Document pages", ) or not wait_for(
            lambda: app.find(R.LABEL, "Accessible sample"), 10):
        print("FAIL  the document did not open")
        return 1
    check_structure(app)
    link = check_reading_order(app)
    check_states_and_actions(app, link)
    if XDOTOOL:
        check_focus(app, pid)
        check_find_and_announcements(app)
        check_menu(app)
    else:
        print("skip  keyboard checks (xdotool not found)")
    check_read_aloud(app)
    if os.environ.get("A11Y_DUMP"):
        for n in app.nodes():
            print("  " * n.path.count("/"), n.role_name, repr(n.name), repr(n.description))
    print(f"{len(failures)} failure(s)")
    return 1 if failures else 0


if __name__ == "__main__":
    sys.exit(enable() if "--enable" in sys.argv else main())
