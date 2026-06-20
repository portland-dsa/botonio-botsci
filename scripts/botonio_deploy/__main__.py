"""Run the tool from a source checkout: `python -m botonio_deploy <verb> ...`.

The bundled .pyz gets its own entry point from zipapp; this is the equivalent for running
straight out of the working tree.
"""

from .cli import main

if __name__ == "__main__":
    main()
