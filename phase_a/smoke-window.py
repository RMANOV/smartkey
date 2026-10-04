import gi
gi.require_version("Gtk", "4.0")
from gi.repository import Gtk
def on_activate(app):
    w = Gtk.ApplicationWindow(application=app, title="PHASE-A SMOKE — TYPE HERE")
    w.set_default_size(560, 360)
    tv = Gtk.TextView(); tv.set_monospace(True); tv.set_wrap_mode(Gtk.WrapMode.WORD)
    sw = Gtk.ScrolledWindow(); sw.set_child(tv); w.set_child(sw)
    w.present(); tv.grab_focus()
app = Gtk.Application(application_id="org.rmanov.PhaseASmoke")
app.connect("activate", on_activate)
app.run(None)
