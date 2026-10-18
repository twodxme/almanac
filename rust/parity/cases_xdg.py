"""Smoke case so the harness itself is exercised before any module is ported."""
import xdg


def cases():
    yield "state_home", {}, lambda: xdg.state_home()
