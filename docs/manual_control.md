# Manual Control and PPM Bridge

Electrode treats manual control as a first-class ground-station workflow with a native hardware boundary.

```text
gamepad / joystick / touch input
  -> electrode-manual-control-bridge
  -> Synapse ManualControlData bare struct
  -> Zenoh key expression: synapse/v1/topic/manual_control_command

autopilot control output
  -> Synapse PwmSignalOutputsData bare struct
  -> Zenoh key expression: synapse/v1/topic/pwm_signal_outputs

synapse/v1/topic/manual_control_command + synapse/v1/topic/pwm_signal_outputs
  -> electrode-ppm-bridge
  -> selected PWM: synapse/motor_output
  -> serial PPM encoder packet
  -> RC receiver
```

The browser or desktop UI owns input selection, arming of the manual-control session, calibration display, and operator feedback. Native bridge binaries own Linux joystick access, Zenoh publishing/subscribing, serial access, and the packet contract used by the encoder hardware.

## Manual Input Bridge

Run the joystick/gamepad bridge from the Electrode workspace:

```bash
npm run manual:bridge -- \
  --device /dev/input/js0 \
  --zenoh-connect udp/127.0.0.1:7447 \
  --topic synapse/v1/topic/manual_control_command
```

Useful options:

```text
--publish-hz 50
--stale-ms 250
--roll-axis 1
--pitch-axis 2
--yaw-axis 3
--throttle-axis 0
--mode-axis 5
--active-axis 4
--invert-active true
--arm-button INDEX
--kill-button INDEX
```

The bridge publishes `synapse.topic.ManualControlData` bare structs. The mode switch selects the output source: manual mode passes transmitter sticks through; auto mode passes autopilot PWM output through. The active/stabilization switch is independent and only drives the stabilization radio channel.

Use the dump tool to inspect published values:

```bash
npm run manual:dump -- --topic synapse/v1/topic/manual_control_command
```

## PPM Bridge

Run the PPM bridge from the Electrode workspace:

```bash
npm run ppm:bridge -- \
  --zenoh-connect udp/127.0.0.1:7447 \
  --manual-topic synapse/v1/topic/manual_control_command \
  --control-output-topic synapse/v1/topic/pwm_signal_outputs \
  --pwm-output-topic synapse/motor_output \
  --serial-device /dev/ttyACM0 \
  --baud-rate 57600
```

Useful environment variables:

```bash
JOYSTICK_DEVICE=/dev/input/js0
ZENOH_CONNECT=udp/127.0.0.1:7447
ZENOH_TOPIC=synapse/v1/topic/manual_control_command
ZENOH_CONTROL_OUTPUT_TOPIC=synapse/v1/topic/pwm_signal_outputs
ZENOH_PWM_OUTPUT_TOPIC=synapse/motor_output
PPM_SERIAL_DEVICE=/dev/ttyACM0
PPM_BAUD_RATE=57600
PPM_CHANNEL_MAP=0,1,2,3,4
```

The PPM bridge selection rules are:

```text
ManualControl valid=true, kill_switch=false, flight_mode=0 -> manual channels
ManualControl valid=true, kill_switch=false, flight_mode>0 -> latest autopilot PWM channels
ManualControl valid=false or kill_switch=true              -> failsafe channels
```

Base channel order before `PPM_CHANNEL_MAP`:

```text
0 throttle
1 aileron / roll
2 elevator / pitch
3 rudder / yaw
4 stabilization
```

The stabilization channel is operator-owned even in auto mode. The PPM bridge preserves channel 4 from manual input while using autopilot channels 0-3 in auto mode.

`synapse/v1/topic/pwm_signal_outputs` and `synapse/motor_output` are interpreted as `synapse.topic.PwmSignalOutputsData` bare structs. The first five PWM outputs map to throttle, roll, pitch, yaw, stabilization before channel-map/invert settings are applied.

The serial packet is 14 bytes:

```text
0xffff header, five little-endian u16 channels, little-endian u16 checksum
```

The checksum is the wrapping sum of the five transmitted channel values.

## Safety Boundary

Failsafe channels are:

```text
throttle 1000
roll     1500
pitch    1500
yaw      1500
mode     2000
```

Serial device access should remain in the native bridge or Tauri shell, not in the browser.
