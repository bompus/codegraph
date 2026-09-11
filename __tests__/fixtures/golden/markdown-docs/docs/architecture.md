# Architecture

The pipeline is `createWidget` -> `render` -> `paint`.

## Rendering

`render` in [widget.ts](../src/widget.ts) delegates to `paint`.

### Painting

`paint` writes to the canvas. Nothing else touches the canvas.

## Links

- [README](../README.md)
- [Contributing](../CONTRIBUTING.md#repo-setup)
