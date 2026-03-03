# Development Notes

## Troubleshooting Brightness Control

### Issue
The `LG_Buddy_Brightness` script was failing to update the TV's brightness. Specifically, it was using the method `set_current_picture_settings`, which resulted in an `AttributeError`.

### Diagnosis
Investigation of the installed `bscpylgtv` version (0.5.1) and its source code revealed the following:
- `set_current_picture_settings` is no longer a direct method on the `WebOsClient`.
- `set_picture_settings` is available but requires explicit `pic_mode` and `tv_input` arguments, making it cumbersome for scripts that should work regardless of the current input or mode.
- `set_settings` with the `"picture"` category is the most efficient way to update settings like `backlight` (OLED Pixel Brightness) for the **currently active** state.

### Resolution
The scripts were updated to use the following pattern:
```bash
bscpylgtvcommand <tv_ip> set_settings picture '{"backlight": <value>}'
```

### Useful CLI Commands for Debugging
- **Check current input:**
  ```bash
  bscpylgtvcommand <tv_ip> get_input
  ```
- **Check current picture settings:**
  ```bash
  bscpylgtvcommand <tv_ip> get_picture_settings
  ```
- **Test brightness change:**
  ```bash
  bscpylgtvcommand <tv_ip> set_settings picture '{"backlight": 50}'
  ```

## Library Reference
For future maintenance, researchers can inspect the `WebOsClient` methods in:
`/usr/bin/LG_Buddy_PIP/lib/python3.13/site-packages/bscpylgtv/webos_client.py`

### Full Method list (v0.5.1)
These were extracted via direct inspection of the installed library:

```python
_volume_step(endpoint)
async_init()
button(name, checkValid=True)
callback_handler(queue, callback, future)
change_sound_output(output)
channel_down()
channel_up()
click()
close()
close_app(app)
close_web()
command(request_type, uri, payload=None, uid=None)
connect()
connect_handler(res)
consumer_handler(ws)
create(*args, **kwargs)
disconnect()
do_state_update_callbacks()
eject_attached_device(device_id)
enable_tpc_or_gsr(algo, enable=True)
fast_forward()
get_apps(jsonOutput=False)
get_apps_all(jsonOutput=False)
get_attached_devices(types=[], jsonOutput=False)
get_audio_status()
get_calibration_info(jsonOutput=False)
get_channel_info(jsonOutput=False)
get_channels(jsonOutput=False)
get_configs(keys=['tv.model.*'], jsonOutput=False)
get_current_app()
get_current_channel()
get_hello_info(jsonOutput=False)
get_input()
get_inputs(jsonOutput=False)
get_muted()
get_picture_settings(keys=['contrast', 'backlight', 'brightness', 'color'], jsonOutput=False)
get_power_state()
get_services(jsonOutput=False)
get_software_info(jsonOutput=False)
get_sound_output()
get_storage()
get_system_info(jsonOutput=False)
get_system_settings(category='option', keys=['audioGuidance'], jsonOutput=False)
get_volume()
input_button()
input_command(message)
insert_text(text, replace=False)
launch_app(app)
launch_app_with_content_id(app, contentId)
launch_app_with_params(app, params)
luna_request(uri, params)
move(dx, dy, down=0)
open_url(url)
pause()
ping_handler(ws)
play()
power_off()
power_on()
print(message)
reboot()
reboot_soft(webos_ver='')
register_state_update_callback(callback)
request(uri, payload=None, cmd_type='request', uid=None)
rewind()
scroll(dx, dy)
send_delete_key()
send_enter_key()
send_message(message, icon_path=None)
set_apps_state(payload)
set_channel(channel)
set_channel_info_state(channel_info)
set_channels_state(channels)
set_configs(settings)
set_current_app_state(appId)
set_current_channel_state(channel)
set_current_picture_mode(pic_mode)
set_device_info(input, icon, label)
set_device_info_luna(input, icon, label)
set_input(input)
set_inputs_state(extinputs)
set_mute(mute)
set_muted_state(muted)
set_picture_mode(pic_mode, tv_input, dynamic_range='sdr', stereoscopic='2d', category='picture')
set_picture_settings(settings, pic_mode, tv_input, stereoscopic='2d', category='picture', current_app=None)
set_picture_settings_state(picture_settings)
set_power_state(payload)
set_settings(category, settings, current_app=None)
set_sm_white_balance(color_temp, rg, gg, bg, rc=64, gc=64, bc=64)
set_sound_output_state(sound_output)
set_system_picture_mode(pic_mode)
set_system_settings(category, settings, current_app=None)
set_usb_dolby_vision_config(action)
set_volume(volume)
set_volume_state(volume)
show_screen_saver()
sleep(seconds)
stop()
subscribe(callback, uri, payload=None)
subscribe_apps(callback)
subscribe_channel_info(callback)
subscribe_channels(callback)
subscribe_current_app(callback)
subscribe_current_channel(callback)
subscribe_inputs(callback)
subscribe_muted(callback)
subscribe_picture_settings(callback, keys=['contrast', 'backlight', 'brightness', 'color'])
subscribe_power(callback)
subscribe_sound_output(callback)
subscribe_volume(callback)
take_screenshot()
turn_3d_off()
turn_3d_on()
turn_screen_off(webos_ver='')
turn_screen_on(webos_ver='')
volume_down()
volume_up()
```
