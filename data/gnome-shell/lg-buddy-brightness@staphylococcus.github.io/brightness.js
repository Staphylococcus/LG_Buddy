// TV brightness state for the Quick Settings slider. Kept free of GNOME Shell
// imports so it can be tested with plain gjs.
//
// spawn(args) starts `lg-buddy <args>` and returns {result, terminate}, where
// result resolves to trimmed stdout or rejects on failure.
// view is {setValue(fraction), setVisible(visible)}.
export class BrightnessController {
    constructor(spawn, view) {
        this._spawn = spawn;
        this._view = view;
        this._running = new Set();
        this._pending = null;
        this._reading = false;
        this._writing = false;
        this._changes = 0;
        this._destroyed = false;
    }

    async refresh() {
        if (this._reading || this._writing)
            return;
        this._reading = true;
        const changes = this._changes;
        // A slider change made while this read was running wins over its result.
        const stale = () => this._destroyed || changes !== this._changes;
        try {
            const value = parseInt(await this._run(['brightness', 'get']));
            if (stale())
                return;
            if (isNaN(value))
                throw new Error('unexpected `lg-buddy brightness get` output');
            this._view.setValue(value / 100);
            this._view.setVisible(true);
        } catch (e) {
            if (!stale())
                this._hide(e);
        } finally {
            this._reading = false;
        }
    }

    set(fraction) {
        this._changes++;
        this._pending = Math.round(fraction * 100);
        if (!this._writing)
            this._flush();
    }

    // Terminates running commands and ignores anything still in flight.
    destroy() {
        this._destroyed = true;
        this._pending = null;
        for (const proc of this._running)
            proc.terminate();
        this._running.clear();
    }

    // One `set` in flight at a time; the latest slider position always lands.
    async _flush() {
        this._writing = true;
        while (this._pending !== null) {
            const value = this._pending;
            this._pending = null;
            try {
                await this._run(['brightness', 'set', String(value)]);
            } catch (e) {
                if (!this._destroyed) {
                    this._pending = null;
                    this._hide(e);
                }
                break;
            }
        }
        this._writing = false;
    }

    // TV off or unreachable; the next menu open retries.
    _hide(error) {
        console.warn(`lg-buddy-brightness: ${error.message}`);
        this._view.setVisible(false);
    }

    async _run(args) {
        const proc = this._spawn(args);
        this._running.add(proc);
        try {
            return await proc.result;
        } finally {
            this._running.delete(proc);
        }
    }
}
