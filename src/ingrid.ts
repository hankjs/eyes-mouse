import { HOP_DURATION, HOP_LANDING, HOP_TAKEOFF, type Facing, type Gait } from './snake'

// Alpha bounds of selected figures from 英格丽德喵-动作设定图.png.
export const POSES = {
  idle: [5, 3, 249, 296],
  crawlReachLeft: [325, 3, 286, 251],
  crawlStretchLeft: [645, 3, 303, 251],
  crawlGatherLeft: [965, 3, 304, 259],
  jumpReadyLeft: [5, 343, 271, 269],
  jumpRiseLeft: [325, 343, 310, 295],
  walkRight: [645, 343, 256, 318],
  jumpLandLeft: [965, 343, 271, 227],
  jumpAirLeft: [5, 683, 298, 307],
  crawlRight: [325, 683, 290, 242],
  walkLeft: [645, 683, 232, 347],
} as const

// Rear-body points in source pixels, measured on each pose in the atlas.
export const TAIL_ATTACHMENTS = {
  idle: [72, 255],
  crawlReachLeft: [581, 173],
  crawlStretchLeft: [911, 178],
  crawlGatherLeft: [1224, 181],
  jumpReadyLeft: [241, 543],
  jumpRiseLeft: [560, 594],
  walkRight: [704, 605],
  jumpLandLeft: [1197, 522],
  jumpAirLeft: [242, 928],
  crawlRight: [384, 881],
  walkLeft: [799, 969],
} as const satisfies Record<keyof typeof POSES, readonly [number, number]>

interface Pose {
  sprite: keyof typeof POSES
  lift?: number
  tilt?: number
  scaleX?: number
  scaleY?: number
}

export const IDLE_GESTURES = ['nod', 'peek', 'sway'] as const
export type IdleGesture = typeof IDLE_GESTURES[number]

const LEFT_CRAWL = ['crawlReachLeft', 'crawlStretchLeft', 'crawlGatherLeft'] as const

export function movementPose(gait: Gait, facing: Facing, seconds: number, reducedMotion = false): Pose {
  if (gait === 'idle') return { sprite: 'idle' }
  const left = facing === -1
  const walk = left ? 'walkLeft' : 'walkRight'
  const crawl = left ? 'crawlReachLeft' : 'crawlRight'

  if (gait === 'crawl') {
    if (reducedMotion) return { sprite: crawl }
    const phase = (seconds / 0.32) % 1
    const reach = Math.sin(phase * 2 * Math.PI)
    return {
      sprite: left ? LEFT_CRAWL[Math.floor(phase * LEFT_CRAWL.length)] : crawl,
      tilt: facing * 0.045 * reach,
      scaleX: 1 + 0.038 * reach,
      scaleY: 1 - 0.05 * reach,
    }
  }

  if (gait === 'walk') {
    if (reducedMotion) return { sprite: walk }
    const step = Math.sin(seconds * 2 * Math.PI / 0.25)
    return { sprite: walk, tilt: facing * (0.025 + 0.075 * step), lift: 2.7 * Math.abs(step) }
  }

  if (reducedMotion) return { sprite: walk }
  const hop = seconds % HOP_DURATION
  if (hop < HOP_TAKEOFF) {
    const crouch = hop / HOP_TAKEOFF
    return { sprite: left ? 'jumpReadyLeft' : walk, scaleX: 1 + 0.07 * crouch, scaleY: 1 - 0.18 * crouch }
  }
  if (hop < HOP_LANDING) {
    const flight = (hop - HOP_TAKEOFF) / (HOP_LANDING - HOP_TAKEOFF)
    const sprite = left ? (flight < 0.45 ? 'jumpRiseLeft' : 'jumpAirLeft') : crawl
    return { sprite, lift: 76 * flight * (1 - flight), tilt: facing * (flight - 0.5) * 0.37 }
  }
  const recovery = (hop - HOP_LANDING) / (HOP_DURATION - HOP_LANDING)
  return {
    sprite: left ? 'jumpLandLeft' : walk,
    scaleX: 1 + 0.09 * (1 - recovery),
    scaleY: 1 - 0.20 * (1 - recovery),
  }
}

export function idlePose(gesture: IdleGesture, facing: Facing, progress: number): Pose {
  const ease = Math.sin(Math.PI * progress) ** 2
  if (gesture === 'nod') return { sprite: 'idle', scaleY: 1 - 0.06 * ease, tilt: 0.036 * ease }
  if (gesture === 'sway') return { sprite: 'idle', tilt: 0.08 * ease * Math.cos(Math.PI * progress) }
  if (progress > 0.20 && progress < 0.80) {
    return facing === -1
      ? { sprite: 'walkLeft', scaleY: 0.86, tilt: -0.04 * ease }
      : { sprite: 'walkRight', scaleY: 0.93, tilt: 0.04 * ease }
  }
  return { sprite: 'idle', tilt: facing * 0.08 * ease }
}
