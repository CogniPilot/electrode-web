import { DEFAULT_VEHICLE_ID, resolveTopic, resolveTopicDefinition, TOPIC_DEFINITIONS } from './topics';
import type { GcsFrame } from './types';

export type PlotValueType = 'number' | 'boolean';
export type PlotPacketSource = 'electrode' | 'synapse_fbs' | 'live';

export interface PlotFieldDefinition {
  path: string;
  label: string;
  units: string;
  valueType: PlotValueType;
}

export interface PlotPacketDefinition {
  key: string;
  topic: string;
  schema: string;
  label: string;
  source: PlotPacketSource;
  fields: PlotFieldDefinition[];
  active: boolean;
  samples: number;
}

export interface PlotSample {
  timeMs: number;
  value: number;
}

export interface PlotSeries {
  key: string;
  packetKey: string;
  topic: string;
  schema: string;
  packetLabel: string;
  fieldPath: string;
  fieldLabel: string;
  units: string;
  valueType: PlotValueType;
  samples: PlotSample[];
  lastValue: number | null;
  updatedAtMs: number;
}

export interface PlotState {
  packets: PlotPacketDefinition[];
  series: PlotSeries[];
  updatedAtMs: number;
}

export interface PlotSeriesUpdate {
  packetKey: string;
  topic: string;
  schema: string;
  packetLabel: string;
  source: PlotPacketSource;
  field: PlotFieldDefinition;
  timeMs: number;
  value: number;
}

interface KnownPacketTemplate {
  topic: string;
  schema: string;
  label: string;
  source: 'electrode' | 'synapse_fbs';
  fields: PlotFieldDefinition[];
}

const ELECTRODE_PACKET_TEMPLATES: KnownPacketTemplate[] = [
  {
    topic: TOPIC_DEFINITIONS.pose.topic,
    schema: 'Pose',
    label: 'Pose',
    source: 'electrode',
    fields: [
      numberField('lat', 'Latitude', 'deg'),
      numberField('lon', 'Longitude', 'deg'),
      numberField('altM', 'Altitude', 'm'),
      numberField('xM', 'Local X', 'm'),
      numberField('yM', 'Local Y', 'm'),
      numberField('zM', 'Local Z', 'm')
    ]
  },
  {
    topic: TOPIC_DEFINITIONS.velocity.topic,
    schema: 'Velocity',
    label: 'Velocity',
    source: 'electrode',
    fields: [
      numberField('northMps', 'North', 'm/s'),
      numberField('eastMps', 'East', 'm/s'),
      numberField('downMps', 'Down', 'm/s'),
      numberField('groundSpeedMps', 'Ground speed', 'm/s')
    ]
  },
  {
    topic: TOPIC_DEFINITIONS.attitude.topic,
    schema: 'Attitude',
    label: 'Attitude',
    source: 'electrode',
    fields: [
      numberField('rollDeg', 'Roll', 'deg'),
      numberField('pitchDeg', 'Pitch', 'deg'),
      numberField('yawDeg', 'Yaw', 'deg')
    ]
  },
  {
    topic: TOPIC_DEFINITIONS.battery.topic,
    schema: 'Battery',
    label: 'Battery',
    source: 'electrode',
    fields: [
      numberField('voltageV', 'Voltage', 'V'),
      numberField('currentA', 'Current', 'A'),
      numberField('remainingPct', 'Remaining', '%')
    ]
  },
  {
    topic: TOPIC_DEFINITIONS.link.topic,
    schema: 'LinkStatus',
    label: 'LinkStatus',
    source: 'electrode',
    fields: [
      numberField('rssiDbm', 'RSSI', 'dBm'),
      numberField('latencyMs', 'Latency', 'ms'),
      numberField('packetLossPct', 'Packet loss', '%')
    ]
  },
  {
    topic: TOPIC_DEFINITIONS.mode.topic,
    schema: 'ModeState',
    label: 'ModeState',
    source: 'electrode',
    fields: [booleanField('armed', 'Armed'), booleanField('failsafe', 'Failsafe')]
  },
  {
    topic: TOPIC_DEFINITIONS.localization.topic,
    schema: 'LocalizationState',
    label: 'Localization',
    source: 'electrode',
    fields: [
      booleanField('fresh', 'Fresh'),
      numberField('quality', 'Quality', ''),
      numberField('updatedAtMs', 'Updated', 'ms')
    ]
  }
];

const SYNAPSE_PACKET_TEMPLATES: KnownPacketTemplate[] = [
  {
    topic: 'external_pose/1',
    schema: 'synapse.topic.ExternalOdometry',
    label: 'ExternalOdometry CUB1',
    source: 'synapse_fbs',
    fields: [
      numberField('data.timestamp_us', 'Timestamp', 'us'),
      ...vec3Fields('data.position', 'Position ENU', 'm'),
      ...quatFields('data.attitude', 'Attitude'),
      ...vec3Fields('data.linear_velocity', 'Linear velocity ENU', 'm/s'),
      ...rateFields('data.angular_velocity', 'Angular velocity FLU'),
      numberField('data.flags', 'Flags', ''),
      numberField('data.source_id', 'Source ID', ''),
      numberField('data.id', 'Rigid body ID', ''),
      booleanField('data.position_valid', 'Position valid'),
      booleanField('data.attitude_valid', 'Attitude valid'),
      booleanField('data.linear_velocity_valid', 'Linear velocity valid'),
      booleanField('data.angular_velocity_valid', 'Angular velocity valid'),
      booleanField('data.extrapolated', 'Extrapolated'),
      booleanField('data.degraded', 'Degraded'),
      booleanField('data.lost', 'Lost')
    ]
  },
  {
    topic: 'manual',
    schema: 'synapse.topic.ManualControlCommand',
    label: 'ManualControl',
    source: 'synapse_fbs',
    fields: [
      numberField('data.timestamp_us', 'Timestamp', 'us'),
      ...manualAxes('data.axes'),
      ...auxArrayFields('data.aux', 6),
      numberField('data.flight_mode', 'Flight mode', ''),
      booleanField('data.arm_switch', 'Arm switch'),
      booleanField('data.kill_switch', 'Kill switch'),
      booleanField('data.active', 'Active'),
      booleanField('data.valid', 'Valid'),
      numberField('data.buttons', 'Buttons', '')
    ]
  },
  {
    topic: 'att',
    schema: 'synapse.topic.AttitudeEstimate',
    label: 'AttitudeEstimate',
    source: 'synapse_fbs',
    fields: [
      numberField('data.timestamp_us', 'Timestamp', 'us'),
      ...quatFields('data.attitude', 'Attitude'),
      ...rateFields('data.angular_velocity', 'Angular velocity'),
      booleanField('data.attitude_valid', 'Attitude valid'),
      booleanField('data.rates_valid', 'Rates valid')
    ]
  },
  {
    topic: 'health',
    schema: 'synapse.topic.VehicleHealth',
    label: 'VehicleHealth',
    source: 'synapse_fbs',
    fields: [
      numberField('data.timestamp_us', 'Timestamp', 'us'),
      numberField('data.flight_mode', 'Flight mode', ''),
      numberField('data.link_quality_pct', 'Link quality', '%'),
      numberField('data.voltage_battery_v', 'Voltage', 'V'),
      numberField('data.current_battery_a', 'Current', 'A'),
      numberField('data.battery_remaining_pct', 'Battery remaining', '%'),
      booleanField('data.armed', 'Armed'),
      booleanField('data.failsafe', 'Failsafe'),
      numberField('data.system_state', 'System state', ''),
      numberField('data.load_pct', 'Load', '%')
    ]
  },
  {
    topic: 'power',
    schema: 'synapse.topic.PowerStatus',
    label: 'PowerStatus',
    source: 'synapse_fbs',
    fields: [
      numberField('data.timestamp_us', 'Timestamp', 'us'),
      numberField('data.voltage_v', 'Voltage', 'V'),
      numberField('data.current_a', 'Current', 'A'),
      numberField('data.remaining_pct', 'Remaining', '%'),
      booleanField('data.connected', 'Connected'),
      numberField('data.temperature_c', 'Temperature', 'C')
    ]
  },
  {
    topic: 'pwm',
    schema: 'synapse.topic.PwmSignalOutputs',
    label: 'PwmSignalOutputs',
    source: 'synapse_fbs',
    fields: [
      numberField('data.timestamp_us', 'Timestamp', 'us'),
      numberField('data.active_mask', 'Active mask', ''),
      numberField('data.port', 'Port', ''),
      ...motorFields('motors', 'Motor', 'us')
    ]
  },
  {
    topic: 'rc',
    schema: 'synapse.topic.RadioControl',
    label: 'RadioControl',
    source: 'synapse_fbs',
    fields: rcChannelFields('')
  },
  {
    // Compact 28-byte pose republished per rigid body — position + attitude
    // only; markers and timing live on `synapse/mocap/frame`.
    topic: 'synapse/mocap/rigid_body/cub1/pose',
    schema: 'synapse.topic.MocapFrame',
    label: 'MocapPose',
    source: 'synapse_fbs',
    fields: [
      ...vec3Fields('rigid_bodies.0.position', 'Rigid body 0 position', 'm'),
      ...quatFields('rigid_bodies.0.attitude', 'Rigid body 0 attitude'),
      booleanField('rigid_bodies.0.tracking_valid', 'Rigid body 0 valid')
    ]
  },
  {
    topic: 'synapse/mocap/frame',
    schema: 'synapse.topic.MocapFrame',
    label: 'MocapFrame',
    source: 'synapse_fbs',
    fields: [
      numberField('timestamp_us', 'Timestamp', 'us'),
      numberField('frame_number', 'Frame number', ''),
      ...vec3Fields('rigid_bodies.0.position', 'Rigid body 0 position', 'm'),
      ...quatFields('rigid_bodies.0.attitude', 'Rigid body 0 attitude'),
      numberField('rigid_bodies.0.residual', 'Rigid body 0 residual', ''),
      booleanField('rigid_bodies.0.tracking_valid', 'Rigid body 0 valid'),
      ...vec3Fields('labeled_markers.0.position', 'Marker 0 position', 'm'),
      numberField('labeled_markers.0.residual', 'Marker 0 residual', '')
    ]
  },
  {
    topic: 'flow',
    schema: 'synapse.topic.OpticalFlow',
    label: 'OpticalFlow',
    source: 'synapse_fbs',
    fields: [
      numberField('data.timestamp_us', 'Timestamp', 'us'),
      ...vec2Fields('data.pixel_flow', 'Pixel flow', 'px'),
      ...vec3Fields('data.delta_angle', 'Delta angle', 'rad'),
      numberField('data.distance_m', 'Distance', 'm'),
      numberField('data.integration_timespan_us', 'Integration time', 'us'),
      numberField('data.quality', 'Quality', ''),
      numberField('data.max_flow_rate', 'Max flow rate', ''),
      numberField('data.min_ground_distance', 'Min ground distance', 'm'),
      numberField('data.max_ground_distance', 'Max ground distance', 'm')
    ]
  },
  {
    topic: 'flow_vel',
    schema: 'synapse.topic.OpticalFlowVelocity',
    label: 'OpticalFlowVelocity',
    source: 'synapse_fbs',
    fields: [
      numberField('data.timestamp_us', 'Timestamp', 'us'),
      ...vec2Fields('data.vel_body', 'Body velocity', 'm/s'),
      ...vec2Fields('data.vel_ne', 'NE velocity', 'm/s'),
      ...vec2Fields('data.flow_rate_uncompensated', 'Flow rate uncomp', ''),
      ...vec2Fields('data.flow_rate_compensated', 'Flow rate comp', ''),
      ...vec3Fields('data.gyro_rate', 'Gyro rate', 'rad/s')
    ]
  },
  {
    topic: 'tick',
    schema: 'synapse.topic.LockstepTick',
    label: 'LockstepTick',
    source: 'synapse_fbs',
    fields: [
      numberField('data.target_boot_time_us', 'Target boot time', 'us'),
      numberField('data.run_id', 'Run ID', ''),
      numberField('data.sequence', 'Sequence', ''),
      numberField('data.flags', 'Flags', '')
    ]
  }
];

const KNOWN_PACKET_TEMPLATES = [...ELECTRODE_PACKET_TEMPLATES, ...SYNAPSE_PACKET_TEMPLATES];

export function plotPacketKey(topic: string, schema: string): string {
  return `${topic}::${schema}`;
}

export function plotSeriesKey(packetKey: string, fieldPath: string): string {
  return `${packetKey}::${fieldPath}`;
}

export function createPlotPacketCatalog(vehicleId = DEFAULT_VEHICLE_ID): PlotPacketDefinition[] {
  return KNOWN_PACKET_TEMPLATES.map((template) => {
    const topic = template.source === 'electrode' ? resolveTopic(template.topic, vehicleId) : template.topic;
    return {
      key: plotPacketKey(topic, template.schema),
      topic,
      schema: template.schema,
      label: template.label,
      source: template.source,
      fields: template.fields,
      active: false,
      samples: 0
    };
  });
}

export function extractPlotSeriesUpdates(frame: GcsFrame): PlotSeriesUpdate[] {
  const timeMs = Math.max(0, frame.header.sourceTimeNs / 1_000_000);
  const topicDefinition = resolveTopicDefinition(frame.topic);
  const frameSchema = topicDefinition?.schema ?? frame.header.messageType;
  const knownTemplate = findKnownPacketTemplate(frame.topic, frameSchema);
  const schema = knownTemplate?.schema ?? frameSchema;
  const packetLabel = knownTemplate?.label ?? frameSchema;
  const source = knownTemplate?.source ?? 'live';
  const fields = knownTemplate?.fields ?? inferNumericFields(frame.payload);
  const packetKey = plotPacketKey(frame.topic, schema);
  const updates: PlotSeriesUpdate[] = [];

  for (const field of fields) {
    const value = numericValueAtPath(frame.payload, field.path);
    if (value === null) {
      continue;
    }

    updates.push({
      packetKey,
      topic: frame.topic,
      schema,
      packetLabel,
      source,
      field,
      timeMs,
      value
    });
  }

  return updates;
}

function findKnownPacketTemplate(topic: string, schema: string): KnownPacketTemplate | undefined {
  return KNOWN_PACKET_TEMPLATES.find((template) => {
    const schemaMatches = schemaNameMatches(template.schema, schema);
    const topicMatches = template.topic === topic || (template.source === 'electrode' && resolveTopicDefinition(topic)?.schema === template.schema);
    return schemaMatches || topicMatches;
  });
}

function schemaNameMatches(knownSchema: string, frameSchema: string): boolean {
  return normalizeSchemaName(knownSchema) === normalizeSchemaName(frameSchema);
}

function normalizeSchemaName(schema: string): string {
  return schema.split('.').at(-1)?.toLowerCase() ?? schema.toLowerCase();
}

function inferNumericFields(payload: unknown, prefix = '', depth = 0): PlotFieldDefinition[] {
  if (depth > 6 || payload === null || payload === undefined) {
    return [];
  }

  if (typeof payload === 'number' || typeof payload === 'boolean') {
    return prefix ? [typeof payload === 'boolean' ? booleanField(prefix, labelFromPath(prefix)) : numberField(prefix, labelFromPath(prefix), '')] : [];
  }

  if (Array.isArray(payload)) {
    return payload
      .slice(0, 4)
      .flatMap((value, index) => inferNumericFields(value, pathJoin(prefix, String(index)), depth + 1))
      .slice(0, 96);
  }

  if (typeof payload === 'object') {
    return Object.entries(payload as Record<string, unknown>)
      .flatMap(([key, value]) => inferNumericFields(value, pathJoin(prefix, key), depth + 1))
      .slice(0, 96);
  }

  return [];
}

function numericValueAtPath(payload: unknown, path: string): number | null {
  const value = path
    .split('.')
    .filter(Boolean)
    .reduce<unknown>((current, segment) => valueSegment(current, segment), payload);

  if (typeof value === 'number' && Number.isFinite(value)) {
    return value;
  }

  if (typeof value === 'boolean') {
    return value ? 1 : 0;
  }

  return null;
}

function valueSegment(value: unknown, segment: string): unknown {
  if (value === null || value === undefined) {
    return undefined;
  }

  if (Array.isArray(value)) {
    const index = Number(segment);
    return Number.isInteger(index) ? value[index] : undefined;
  }

  if (typeof value !== 'object') {
    return undefined;
  }

  const object = value as Record<string, unknown>;
  return object[segment] ?? object[snakeToCamel(segment)] ?? object[camelToSnake(segment)];
}

function numberField(path: string, label: string, units: string): PlotFieldDefinition {
  return {
    path,
    label,
    units,
    valueType: 'number'
  };
}

function booleanField(path: string, label: string): PlotFieldDefinition {
  return {
    path,
    label,
    units: '0/1',
    valueType: 'boolean'
  };
}

function vec2Fields(prefix: string, label: string, units: string): PlotFieldDefinition[] {
  return ['x', 'y'].map((axis) => numberField(pathJoin(prefix, axis), `${label} ${axis.toUpperCase()}`, units));
}

function vec3Fields(prefix: string, label: string, units: string): PlotFieldDefinition[] {
  return ['x', 'y', 'z'].map((axis) => numberField(pathJoin(prefix, axis), `${label} ${axis.toUpperCase()}`, units));
}

function quatFields(prefix: string, label: string): PlotFieldDefinition[] {
  return ['x', 'y', 'z', 'w'].map((axis) => numberField(pathJoin(prefix, axis), `${label} ${axis.toUpperCase()}`, ''));
}

function rateFields(prefix: string, label: string): PlotFieldDefinition[] {
  return ['roll', 'pitch', 'yaw'].map((axis) => numberField(pathJoin(prefix, axis), `${label} ${axis}`, 'rad/s'));
}

function manualAxes(prefix: string): PlotFieldDefinition[] {
  return [
    numberField(pathJoin(prefix, 'roll'), 'Roll', ''),
    numberField(pathJoin(prefix, 'pitch'), 'Pitch', ''),
    numberField(pathJoin(prefix, 'yaw'), 'Yaw', ''),
    numberField(pathJoin(prefix, 'throttle'), 'Throttle', '')
  ];
}

function auxArrayFields(prefix: string, count: number): PlotFieldDefinition[] {
  return Array.from({ length: count }, (_, index) => numberField(pathJoin(prefix, String(index)), `Aux ${index}`, ''));
}

function rcChannelFields(prefix: string): PlotFieldDefinition[] {
  return Array.from({ length: 16 }, (_, index) => numberField(pathJoin(prefix, `ch${index}`), `RC ch${index}`, 'us'));
}

function motorFields(prefix: string, label: string, units: string): PlotFieldDefinition[] {
  return Array.from({ length: 4 }, (_, index) => numberField(pathJoin(prefix, `m${index}`), `${label} ${index}`, units));
}

function pathJoin(prefix: string, suffix: string): string {
  return prefix ? `${prefix}.${suffix}` : suffix;
}

function labelFromPath(path: string): string {
  return path
    .split('.')
    .at(-1)!
    .replaceAll('_', ' ')
    .replace(/([a-z])([A-Z])/g, '$1 $2')
    .replace(/\b\w/g, (letter) => letter.toUpperCase());
}

function snakeToCamel(value: string): string {
  return value.replace(/_([a-z])/g, (_, letter: string) => letter.toUpperCase());
}

function camelToSnake(value: string): string {
  return value.replace(/[A-Z]/g, (letter) => `_${letter.toLowerCase()}`);
}
