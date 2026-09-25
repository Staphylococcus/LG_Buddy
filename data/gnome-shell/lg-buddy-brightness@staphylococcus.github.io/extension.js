import Gio from 'gi://Gio';

import * as Main from 'resource:///org/gnome/shell/ui/main.js';
import {Extension} from 'resource:///org/gnome/shell/extensions/extension.js';
import {QuickSlider} from 'resource:///org/gnome/shell/ui/quickSettings.js';

function run(args, cancellable) {
    return new Promise((resolve, reject) => {
        const proc = Gio.Subprocess.new(['lg-buddy', ...args],
            Gio.SubprocessFlags.STDOUT_PIPE | Gio.SubprocessFlags.STDERR_MERGE);
        proc.communicate_utf8_async(null, cancellable, (p, res) => {
            try {
                const [, out] = p.communicate_utf8_finish(res);
                if (p.get_successful())
                    resolve(out.trim());
                else
                    reject(new Error(out.trim()));
            } catch (e) {
                reject(e);
            }
        });
    });
}

export default class LgBuddyBrightness extends Extension {
    enable() {
        this._cancellable = new Gio.Cancellable();
        this._pending = null;
        this._busy = false;
        this._updating = false;

        this._item = new QuickSlider({iconName: 'display-brightness-symbolic'});
        this._item.slider.accessible_name = 'TV Brightness';
        this._item.slider.connect('notify::value', () => this._onValue());

        // Same spot GNOME puts its own (hidden, no backlight) brightness slider.
        const qs = Main.panel.statusArea.quickSettings;
        qs.menu.insertItemBefore(this._item, qs._brightness?.quickSettingsItems[0] ?? null, 2);
        qs.menu.connectObject('open-state-changed', (_m, open) => open && this._refresh(), this);

        this._refresh();
    }

    disable() {
        this._cancellable.cancel();
        Main.panel.statusArea.quickSettings.menu.disconnectObject(this);
        this._item.destroy();
        this._item = null;
    }

    async _refresh() {
        if (this._busy)
            return;
        try {
            const value = parseInt(await run(['brightness', 'get'], this._cancellable));
            if (!this._item || this._busy || isNaN(value))
                return;
            this._updating = true;
            this._item.slider.value = value / 100;
            this._updating = false;
            this._item.visible = true;
        } catch (e) {
            if (this._item)
                this._item.visible = false; // TV off/unreachable; retried on next menu open
        }
    }

    _onValue() {
        if (this._updating)
            return;
        this._pending = Math.round(this._item.slider.value * 100);
        if (!this._busy)
            this._flush();
    }

    // One `set` in flight at a time; the latest slider position always lands.
    async _flush() {
        this._busy = true;
        while (this._pending !== null && this._item) {
            const value = this._pending;
            this._pending = null;
            try {
                await run(['brightness', 'set', String(value)], this._cancellable);
            } catch (e) {
                if (this._item)
                    console.error(`lg-buddy-brightness: ${e.message}`);
            }
        }
        this._busy = false;
    }
}
