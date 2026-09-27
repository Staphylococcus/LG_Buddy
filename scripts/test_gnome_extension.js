// Behavior tests for the GNOME Shell extension's brightness controller.
// Run with: gjs -m scripts/test_gnome_extension.js
import {BrightnessController} from '../data/gnome-shell/lg-buddy-brightness@staphylococcus.github.io/brightness.js';

// Failure cases print expected `lg-buddy-brightness:` warnings.

function harness(spawnOverride) {
    const calls = [];
    const view = {
        value: null,
        visible: null,
        setValue(value) {
            this.value = value;
        },
        setVisible(visible) {
            this.visible = visible;
        },
    };
    const spawn = spawnOverride ?? (args => {
        const call = {args: args.join(' '), terminated: false};
        const result = new Promise((resolve, reject) => {
            call.resolve = resolve;
            call.reject = reject;
        });
        call.terminate = () => {
            call.terminated = true;
            call.reject(new Error('terminated'));
        };
        calls.push(call);
        return {result, terminate: call.terminate};
    });
    return {calls, view, controller: new BrightnessController(spawn, view)};
}

async function settle() {
    for (let i = 0; i < 20; i++)
        await null;
}

function assertEqual(actual, expected, what) {
    if (actual !== expected)
        throw new Error(`${what}: expected ${JSON.stringify(expected)}, got ${JSON.stringify(actual)}`);
}

function assertArgs(calls, expected) {
    assertEqual(JSON.stringify(calls.map(c => c.args)), JSON.stringify(expected), 'commands');
}

const tests = {
    async 'refresh shows the TV value'() {
        const {calls, view, controller} = harness();
        controller.refresh();
        assertArgs(calls, ['brightness get']);
        calls[0].resolve('40');
        await settle();
        assertEqual(view.value, 0.4, 'slider value');
        assertEqual(view.visible, true, 'visible');
    },

    async 'rapid changes keep one write in flight and apply the last value'() {
        const {calls, controller} = harness();
        controller.set(0.1);
        controller.set(0.2);
        controller.set(0.3);
        assertArgs(calls, ['brightness set 10']);
        calls[0].resolve('');
        await settle();
        assertArgs(calls, ['brightness set 10', 'brightness set 30']);
        calls[1].resolve('');
        await settle();
        assertEqual(calls.length, 2, 'command count');
    },

    async 'a read that finishes after a slider change is ignored'() {
        const {calls, view, controller} = harness();
        controller.refresh();
        controller.set(0.7);
        assertArgs(calls, ['brightness get', 'brightness set 70']);
        calls[1].resolve('');
        await settle();
        calls[0].resolve('20');
        await settle();
        assertEqual(view.value, null, 'slider value');
        assertEqual(view.visible, null, 'visible');
    },

    async 'refresh is skipped while a write is running'() {
        const {calls, controller} = harness();
        controller.set(0.5);
        controller.refresh();
        assertArgs(calls, ['brightness set 50']);
    },

    async 'a failed read hides the slider and the next refresh restores it'() {
        const {calls, view, controller} = harness();
        controller.refresh();
        calls[0].reject(new Error('TV unreachable'));
        await settle();
        assertEqual(view.visible, false, 'visible after failure');
        controller.refresh();
        calls[1].resolve('55');
        await settle();
        assertEqual(view.visible, true, 'visible after retry');
        assertEqual(view.value, 0.55, 'slider value');
    },

    async 'a failed write hides the slider and drops pending values'() {
        const {calls, view, controller} = harness();
        controller.set(0.5);
        controller.set(0.6);
        calls[0].reject(new Error('TV unreachable'));
        await settle();
        assertEqual(view.visible, false, 'visible');
        assertArgs(calls, ['brightness set 50']);
    },

    async 'unparseable output hides the slider'() {
        const {calls, view, controller} = harness();
        controller.refresh();
        calls[0].resolve('not a number');
        await settle();
        assertEqual(view.visible, false, 'visible');
    },

    async 'a missing lg-buddy binary hides the slider'() {
        const {view, controller} = harness(() => {
            throw new Error('No such file or directory');
        });
        controller.refresh();
        await settle();
        assertEqual(view.visible, false, 'visible');
    },

    async 'destroy terminates running commands and ignores their results'() {
        const {calls, view, controller} = harness();
        controller.refresh();
        controller.set(0.3);
        controller.set(0.4);
        controller.destroy();
        assertEqual(calls.every(c => c.terminated), true, 'all terminated');
        await settle();
        assertArgs(calls, ['brightness get', 'brightness set 30']);
        assertEqual(view.value, null, 'slider value');
        assertEqual(view.visible, null, 'visible');
    },
};

for (const [name, test] of Object.entries(tests)) {
    await test();
    print(`ok - ${name}`);
}
print(`All ${Object.keys(tests).length} GNOME extension behavior tests passed.`);
