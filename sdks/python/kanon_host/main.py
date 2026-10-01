"""Kanon Official Python Host Process.

Loads one plugin from its manifest or script and serves it with :class:`kanon_sdk.KanonHost`
over the IPC socket the supervisor assigned (``KANON_HOST_SOCK``).
"""

import argparse
import importlib.util
import inspect
import sys
from pathlib import Path

# Ensure sdks/python is on sys.path
_current_dir = Path(__file__).resolve().parent
_python_sdk_dir = _current_dir.parent
if str(_python_sdk_dir) not in sys.path:
    sys.path.insert(0, str(_python_sdk_dir))

from kanon_sdk.host import KanonHost
from kanon_sdk.plugin import Plugin


def load_plugin_from_path(target_path: Path) -> Plugin:
    """Loads a Plugin subclass from a Python script or plugin.toml directory."""
    script_path = target_path
    if target_path.is_file() and target_path.name == "plugin.toml":
        import tomllib
        with open(target_path, "rb") as f:
            data = tomllib.load(f)
        entrypoint = data.get("plugin", {}).get("entrypoint", "main.py")
        script_path = target_path.parent / entrypoint
    elif target_path.is_dir():
        manifest = target_path / "plugin.toml"
        if manifest.exists():
            import tomllib
            with open(manifest, "rb") as f:
                data = tomllib.load(f)
            entrypoint = data.get("plugin", {}).get("entrypoint", "main.py")
            script_path = target_path / entrypoint
        else:
            script_path = target_path / "main.py"

    if not script_path.exists():
        raise FileNotFoundError(f"Plugin entrypoint script not found: {script_path}")

    # Add plugin directory to sys.path so it can import sibling modules
    plugin_dir = str(script_path.parent)
    if plugin_dir not in sys.path:
        sys.path.insert(0, plugin_dir)

    module_name = f"kanon_plugin_{script_path.stem}"
    spec = importlib.util.spec_from_file_location(module_name, script_path)
    if spec is None or spec.loader is None:
        raise ImportError(f"Cannot load module spec from {script_path}")

    module = importlib.util.module_from_spec(spec)
    sys.modules[module_name] = module
    spec.loader.exec_module(module)

    # Search module for Plugin instance or subclass
    for _, attr in inspect.getmembers(module):
        if inspect.isclass(attr) and issubclass(attr, Plugin) and attr is not Plugin:
            return attr()
        if isinstance(attr, Plugin):
            return attr

    raise ValueError(f"No Plugin class or instance found in {script_path}")


def main() -> None:
    parser = argparse.ArgumentParser(description="Kanon Python Plugin Host Runner")
    parser.add_argument("--plugin", type=str, required=True, help="Path to plugin manifest or script")
    parser.add_argument("--socket", type=str, default=None, help="Path to host IPC socket")
    args = parser.parse_args()

    plugin = load_plugin_from_path(Path(args.plugin).resolve())
    KanonHost(plugin, socket_path=args.socket).run()


if __name__ == "__main__":
    main()
