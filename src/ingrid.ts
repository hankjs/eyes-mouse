import type { Facing, Gait } from './snake.ts'

// Alpha bounds of selected figures from 英格丽德喵-动作设定图.png.
export const POSES = {
  idle: [5, 3, 249, 296],
  crawlReachLeft: [325, 3, 286, 251],
  crawlStretchLeft: [645, 3, 303, 251],
  crawlGatherLeft: [965, 3, 304, 259],
  walkRight: [645, 343, 256, 318],
  peek: [965, 343, 271, 227],
  jumpAirLeft: [5, 683, 298, 307],
  crawlRight: [325, 683, 290, 242],
  walkLeft: [645, 683, 232, 347],
} as const

export interface IdleAccent {
  kind: 'heart' | 'snack'
  x: number
  y: number
  radius: number
  opacity: number
}

interface Pose {
  sprite: keyof typeof POSES
  lift?: number
  tilt?: number
  scaleX?: number
  scaleY?: number
  accent?: IdleAccent
}

export const IDLE_GESTURES = ['heart', 'snack', 'peek'] as const
export type IdleGesture = typeof IDLE_GESTURES[number]

const CHASE_STYLES = ['run', 'crawl', 'fly'] as const
type ChaseStyle = typeof CHASE_STYLES[number]

// Allow brief rests between the native pointer samples to belong to the same chase.
const CHASE_PAUSE_MS = 180
const FLIGHT_RISE = 0.14
const FLIGHT_LANDING = 0.18
const TURN = 2 * Math.PI
const LEFT_CRAWL = ['crawlReachLeft', 'crawlStretchLeft', 'crawlGatherLeft'] as const

function unit(value: number) {
  return Math.min(1, Math.max(0, value))
}

function ease(value: number) {
  const t = unit(value)
  return t * t * (3 - 2 * t)
}

export function pickAnimation<T>(options: readonly T[], previous: T | undefined, random = Math.random): T {
  const choices = options.filter((option) => option !== previous)
  return choices[Math.floor(random() * choices.length)]
}

export class ChaseAnimation {
  private style: ChaseStyle | undefined
  private lastMovingAt = -Infinity
  private seconds = 0
  private flight = 0
  private moving = false

  constructor(private readonly random = Math.random) {}

  advance(now: number, seconds: number, moving: boolean, reduced: boolean) {
    if (moving) {
      if (!this.moving && now - this.lastMovingAt > CHASE_PAUSE_MS) {
        this.style = pickAnimation(CHASE_STYLES, this.style, this.random)
        this.seconds = 0
        this.flight = 0
      }
      this.lastMovingAt = now
    }
    this.moving = moving
    const takingOff = moving && this.style === 'fly'
    this.flight = reduced ? 0 : unit(this.flight + (takingOff ? seconds / FLIGHT_RISE : -seconds / FLIGHT_LANDING))
    if (moving || this.flight > 0) this.seconds += seconds
    return this.flight > 0
  }

  stop() {
    this.lastMovingAt = -Infinity
    this.seconds = 0
    this.flight = 0
    this.moving = false
  }

  pose(facing: Facing, gait: Gait, reduced: boolean): Pose {
    if (reduced) {
      if (gait === 'idle') return { sprite: 'idle' }
      if (gait === 'crawl') return { sprite: facing === -1 ? 'crawlReachLeft' : 'crawlRight' }
      return { sprite: facing === -1 ? 'walkLeft' : 'walkRight' }
    }
    if (this.flight > 0 || (this.moving && this.style === 'fly')) {
      const bob = Math.sin(this.seconds * TURN / 0.52)
      const height = ease(this.flight)
      return {
        sprite: facing === -1 ? 'jumpAirLeft' : 'crawlRight',
        lift: (20 + 2.5 * bob) * height,
        tilt: facing * (0.10 + 0.055 * bob) * height,
        scaleY: 1 + 0.025 * bob * height,
      }
    }
    if (!this.moving) return { sprite: 'idle' }
    return groundPose(this.style === 'crawl', facing, this.seconds)
  }
}

function groundPose(crawling: boolean, facing: Facing, seconds: number): Pose {
  const left = facing === -1

  if (crawling) {
    const phase = (seconds / 0.38) % 1
    const reach = Math.sin(phase * TURN)
    return {
      sprite: left ? LEFT_CRAWL[Math.floor(phase * LEFT_CRAWL.length)] : 'crawlRight',
      tilt: facing * 0.052 * reach,
      scaleX: 1 + 0.052 * reach,
      scaleY: 0.90 - 0.068 * reach,
    }
  }

  const stride = Math.sin(seconds * TURN / 0.28)
  const spring = stride ** 2
  return {
    sprite: left ? 'walkLeft' : 'walkRight',
    tilt: facing * (0.085 + 0.055 * stride),
    lift: 4.3 * spring,
    scaleX: 1.035 - 0.06 * spring,
    scaleY: 0.94 + 0.095 * spring,
  }
}

export function idlePose(gesture: IdleGesture, facing: Facing, progress: number): Pose {
  const envelope = Math.sin(Math.PI * progress) ** 2
  if (gesture === 'heart') {
    const reveal = ease((progress - 0.09) / 0.21)
    const opacity = reveal * (1 - ease((progress - 0.61) / 0.34))
    return {
      sprite: 'idle',
      tilt: facing * 0.10 * envelope,
      scaleY: 1 - 0.055 * envelope,
      accent: opacity > 0 ? {
        kind: 'heart',
        x: facing * (21 + 5 * progress),
        y: -61 - 10 * progress,
        radius: 6.4 * reveal,
        opacity,
      } : undefined,
    }
  }
  if (gesture === 'snack') {
    const bite = ease((progress - 0.17) / 0.35)
    const chewing = unit((progress - 0.44) / 0.51)
    const chew = Math.sin(3 * Math.PI * chewing) ** 2 * Math.sin(Math.PI * chewing)
    const opacity = ease(progress / 0.11) * (1 - ease((progress - 0.40) / 0.13))
    return {
      sprite: 'idle',
      tilt: facing * (0.042 * envelope - 0.042 * chew),
      scaleX: 1 + 0.04 * chew,
      scaleY: 1 - 0.10 * chew,
      accent: opacity > 0 ? {
        kind: 'snack',
        x: facing * 19 * (1 - bite),
        y: -17 - 7 * bite,
        radius: 4.3 * (1 - 0.6 * bite),
        opacity,
      } : undefined,
    }
  }
  const crouch = ease(progress / 0.25) * ease((1 - progress) / 0.25)
  const tilt = facing * 0.06 * envelope * Math.cos(Math.PI * progress)
  if (progress > 0.25 && progress < 0.75) {
    return { sprite: 'peek', tilt, scaleX: 0.97, scaleY: 1.03 }
  }
  return { sprite: 'idle', tilt, scaleX: 1 + 0.05 * crouch, scaleY: 1 - 0.21 * crouch }
}
