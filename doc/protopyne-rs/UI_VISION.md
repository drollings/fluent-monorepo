Here is an optimized technical design description and specification for a coding agent to build the stock ticker charting UI, based on the provided video and requirements.

---

# Technical Specification: Stock Ticker Charting UI

## 1. Architecture & Layout Overview

* **Layout Structure**:

* **Top Bar**: Contains the ticker symbol button (showing the loaded ticker,
  defaulting to "SPY"), an indicator selector, the timeframe selector right
  of it, a display-mode pulldown, a `Range` button opening an interactive
  range dialog (presets 1M/3M/YTD/1Y/5Y/All atop a Beginning calendar and a
  Jump To Time field, applied with OK), refresh / AutoHeight buttons, and a
  label-free rewind-to-beginning / rewind / forward / forward-to-last pan
  cluster (each pan refits AutoHeight). Every control shares one height and
  one bold label style (Material app-bar emphasis). The chart loads a
  one-month window at the five-minute default timeframe; larger ranges, other
  tickers, and a Beginning pick stream on demand, always relative to the
  latest loaded bar.

* **Time Grid**: The boundary lines and the pane-divider labels show only
  major time units (hour, four hours, day, week, month) — never an arbitrary
  minute like `23:05`. The unit is the next sensible boundary above the
  chart's own magnitude (1M/5M hourly, 15M/1H every four hours, 4H daily, 1D
  weekly), and each label reads at the detail of the largest unit elapsed: a
  day change shows `MM/DD HH:MM`, an hour change `HH:MM`.

* **Catalog Starring & Persistence**: The symbol and indicator catalogue rows
  carry a favorite star (a hollow `☆` until starred, a filled accent `★`
  afterwards) that toggles a persisted favorites list, so the modal's
  Favorites filter means what it says. Favorites, the indicator loadout, and
  non-default chart settings serialize to `protopyne.json` beside the
  historical/indicator/library roots and reload on boot.

* **Main Chart Area**: The primary candlestick/line rendering surface with price scale on the right axis and time scale along the bottom.

* **Sub-Pane Area**: A secondary, vertically stacked sub-pane allocated for oscillator-based indicators (e.g., "Integration Lite").

* **Overlay Legend / Hotlist**: Upper-left / upper-right overlay showing active indicators with individual toggle controls (eye icons) and configuration modals (six-sided bolt icons).

---

## 2. Core UI Interactions & Viewport Controls

* **Pan & Zoom**:

* **Left-Click-and-Hold Pan**: Dragging the cursor across the canvas shifts the time and price domain values smoothly.

* **Mouse Wheel Zoom**: Scrolling in/out dynamically scales the time axis (zooming candles in or out horizontally) centered around the mouse cursor position.

* **Symbol Selection Modal**:

* Triggered via the symbol search button in the top bar.

* Displays a search bar with real-time query filtering across asset classes (Stocks, Funds, Futures, Crypto, etc.) and a categorized results list.

* **Indicator Search & Management Modal**:

* Triggered via the top-bar indicator button.

* Opens a searchable library modal with tabs (Favorites, My Scripts, Technical, Financial, etc.) to hot-slot indicators into the upper-right control stack.

---

## 3. Indicator System Architecture

* **Modular Add-On Structure**: Indicators are encapsulated as separate plug-and-play modules that extend the base charting UI via custom overlay layers, line series, or sub-panes.

* **Hot-Slotted Control Stack (Upper-Right/Upper-Left Overlay)**:

* Each active indicator has an individual entry displaying its name, active values/readouts, an **Eye Icon** for visibility toggling (activating/deactivating rendering instantly), and a **Six-Sided Bolt Icon** to open its settings/control pane modal.

* **Indicator Isolation**: Indicators must be architecturally decoupled (e.g., "Integration Lite" and "Fast MAs" operate as distinct, isolated modules with independent settings).

---

## 4. Specific Indicator Specification: "Integration Lite"

* **Main Pane Add-ons**:

* **Trend Ribbon**: A dynamic multi-color trend ribbon (rendered in teal and purple variations) mapped across the primary price candles.

* **Sub-Pane Specifications**:

* **Oscillator Pane**: A dedicated lower panel housing four separate indicator readouts (e.g., RSI, CCI, etc.).

* **Normalization & Scaling ($\tanh$)**: All individual oscillator readouts must be normalized to a zero-centered scale and bounded between **+100 and -100** using a hyperbolic tangent ($\tanh$) or equivalent scaling function to establish uniform overbought and oversold zones.

* **Delta Volume Histogram**: A centered delta volume histogram overlaid directly on the zero axis of the lower oscillator sub-pane.

* **Indicator Settings Control Pane**:

* Accessible via the six-sided bolt icon.

* Includes input configuration tabs (`Inputs`, `Style`, `Visibility`), allowing users to tune lengths, signal smoothing parameters, source types, color palettes, and sub-pane thresholds.

---

## 5. Instructions for the Coding Agent

1. **Canvas & Layout Engine**: Build a responsive flex container with a top navigation bar and a multi-pane charting layout. Use an optimized rendering loop (Canvas API) for 60fps pan and wheel-zoom performance.

2. **State Management**: Implement a central store for the active ticker symbol, timeframe, active indicator hotlist, and individual indicator parameter states.

3. **Indicator Plugin Interface**: Define a standard interface for indicators (`IIndicator`) that supports rendering hooks for both main chart overlays and separate sub-panes.

4. **Data Pipeline Integration**: Bind the UI actions (ticker dropdown, timeframe selection, indicator settings modal inputs) to backend API endpoints for historical OHLCV data and indicator calculation payloads.