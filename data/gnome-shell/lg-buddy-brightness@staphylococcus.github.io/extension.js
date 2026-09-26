import Gio from 'gi://Gio';

import * as Main from 'resource:///org/gnome/shell/ui/main.js';
import {Extension} from 'resource:///org/gnome/shell/extensions/extension.js';
import {QuickSlider} from 'resource:///org/gnome/shell/ui/quickSettings.js';

import {BrightnessController} from './brightness.js';

function spawn(args) {
    const proc = Gio.Subprocess.new(['lg-buddy', ...args],
        Gio.SubprocessFlags.STDOUT_PIPE | Gio.SubprocessFlags.STDERR_MERGE);
    const result = new Promise((resolve, reject) => {
        proc.communicate_utf8_async(null, null, (p, res) => {
            try {
                const [, out] = p.communicate_utf8_finish(res);
                if (p.get_successful())
                    resolve(out.trim());
                else
                    reject(new Error(out.trim() || `lg-buddy ${args.join(' ')} failed`));
            } catch (e) {
                reject(e);
            }
        });
    });
    return {result, terminate: () => proc.force_exit()};
}

export default class LgBuddyBrightness extends Extension {
    enable() {
        // A TV icon keeps it distinct from GNOME's own brightness slider on
        // machines that also have a built-in display.
        this._item = new QuickSlider({iconName: 'tv-symbolic'});
        this._item.slider.accessible_name = 'TV Brightness';

        let updating = false;
        this._controller = new BrightnessController(spawn, {
            setValue: value => {
                updating = true;
                this._item.slider.value = value;
                updating = false;
            },
            setVisible: visible => {
                this._item.visible = visible;
            },
        });
        this._item.slider.connect('notify::value', () => {
            if (!updating)
                this._controller.set(this._item.slider.value);
        });

        // Same spot GNOME puts its own brightness slider.
        const qs = Main.panel.statusArea.quickSettings;
        qs.menu.insertItemBefore(this._item, qs._brightness?.quickSettingsItems[0] ?? null, 2);
        qs.menu.connectObject('open-state-changed',
            (_menu, open) => open && this._controller.refresh(), this);

        this._controller.refresh();
    }

    disable() {
        this._controller.destroy();
        Main.panel.statusArea.quickSettings.menu.disconnectObject(this);
        this._item.destroy();
        this._item = null;
        this._controller = null;
    }
}
