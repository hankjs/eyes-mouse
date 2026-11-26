import type { Food, Point } from './snake.ts'

export type LightTier = 0 | 1 | 2

export const LIGHT_TIERS = [
  { radius: 7, orbitRadius: 5, response: 0.14, catchTime: 0.68, trailLength: 22 },
  { radius: 10, orbitRadius: 7, response: 0.23, catchTime: 1.04, trailLength: 30 },
  { radius: 14, orbitRadius: 10, response: 0.34, catchTime: 1.55, trailLength: 38 },
] as const

export interface LightAnchor extends Point {
  scale: number
  radiusX: number
  radiusY: number
}

export interface OrbitLight extends Point {
  id: number
  tier: LightTier
  trailX: number
  trailY: number
  opacity: number
}

export interface LightEffect extends Point {
  kind: 'merge' | 'seal'
  tier: LightTier
  progress: number
  sources: { x: number; y: number; tier: LightTier }[]
}

interface ChasingLight extends OrbitLight {
  slot: number
  centerX: number
  centerY: number
  remaining: number
}

interface TimedEffect extends LightEffect {
  elapsed: number
  duration: number
  inputs: ChasingLight[]
  result: ChasingLight | null
}

const CIRCLE = 2 * Math.PI
const ORBIT_SLOTS = 8
const ORBIT_SPEED = 1.1
const HALO_SCALE = 1.7
const MERGE_DURATION = 0.36
const SEAL_DURATION = 1.05

function fit(value: number, viewport: number, margin: number) {
  const inset = Math.min(viewport / 2, margin)
  return Math.max(inset, Math.min(viewport - inset, value))
}

function fitOrbit(center: number, radius: number, viewport: number, margin: number, direction: number) {
  const span = Math.min(radius, Math.max(0, viewport / 2 - margin))
  return fit(center, viewport, margin + span) + direction * span
}

export class LightOrbit {
  readonly lights: ChasingLight[] = []
  readonly effects: TimedEffect[] = []
  anchor: LightAnchor | null = null

  private phase = -0.65
  private nextId = 0
  private reduced = false

  constructor(private readonly width: number, private readonly height: number) {
    const center = { x: width / 2, y: height / 2 }
    this.append(0, center)
    this.append(0, center)
  }

  get waitingLights(): OrbitLight[] {
    return this.reduced ? [] : this.effects.flatMap((effect) => effect.progress === 0 ? effect.inputs : [])
  }

  add(food: Food) {
    this.append(food.tier, food)
    let tier = food.tier
    while (true) {
      const group = this.lights.filter((light) => light.tier === tier)
      if (group.length < 3) return
      // Let these lights finish forming before displaying their next transformation.
      const delay = this.effects.reduce((pending, effect) => effect.result && group.includes(effect.result)
        ? Math.max(pending, effect.duration - effect.elapsed) : pending, 0)
      for (const light of group) this.lights.splice(this.lights.indexOf(light), 1)
      if (tier === 2) {
        this.effect('seal', tier, this.anchor ?? food, group, delay, null)
        return
      }
      const point = {
        x: group.reduce((total, source) => total + source.x, 0) / group.length,
        y: group.reduce((total, source) => total + source.y, 0) / group.length,
      }
      tier = tier === 0 ? 1 : 2
      const light = this.append(tier, point)
      if (!this.reduced) {
        light.opacity = 0
        this.effect('merge', tier, point, group, delay, light)
      }
    }
  }

  overlaps(point: Point, clearance: number) {
    const scale = this.anchor?.scale
    return scale !== undefined && this.lights.some((light) =>
      Math.hypot(point.x - light.x, point.y - light.y) < clearance + this.margin(light.tier, scale),
    )
  }

  follow(anchor: LightAnchor, seconds: number, { moving, reduced, snap = false }: {
    moving: boolean
    reduced: boolean
    snap?: boolean
  }): boolean {
    const elapsed = Math.max(0, seconds)
    const previous = this.anchor
    const settle = snap || reduced || !previous || previous.scale !== anchor.scale
    const displaced = previous !== null && Math.hypot(anchor.x - previous.x, anchor.y - previous.y) > 8 * anchor.scale
    this.anchor = anchor
    this.reduced = reduced
    if (!reduced) this.phase = (this.phase + elapsed * ORBIT_SPEED) % CIRCLE

    for (const light of this.lights) {
      this.move(light, anchor, elapsed, moving || displaced, settle)
      light.opacity = 1
    }
    for (let index = this.effects.length - 1; index >= 0; index--) {
      const effect = this.effects[index]
      if (effect.elapsed < 0 && !reduced) {
        for (const [index, light] of effect.inputs.entries()) {
          this.move(light, anchor, Math.min(elapsed, -effect.elapsed), moving || displaced, settle)
          effect.sources[index].x = light.x
          effect.sources[index].y = light.y
        }
      }
      if (reduced && effect.elapsed < 0) effect.elapsed = 0
      effect.elapsed += elapsed
      if (effect.elapsed >= effect.duration || (reduced && effect.kind === 'merge')) {
        if (effect.result) effect.result.opacity = 1
        this.effects.splice(index, 1)
        continue
      }
      if (effect.kind === 'seal' && effect.progress === 0) this.placeSeal(effect, anchor)
      effect.progress = Math.max(0, effect.elapsed / effect.duration)
      if (effect.result) {
        effect.x = effect.result.x
        effect.y = effect.result.y
        effect.result.opacity = Math.max(0, (effect.progress - 0.42) / 0.58)
      }
    }
    return this.effects.length > 0 || (!reduced && this.lights.length > 0)
  }

  private append(tier: LightTier, point: Point) {
    let slot = 0
    let mostSpace = 0
    for (let candidate = 0; candidate < ORBIT_SLOTS; candidate++) {
      let space = ORBIT_SLOTS
      for (const light of this.lights) {
        const gap = Math.abs(candidate - light.slot)
        space = Math.min(space, gap, ORBIT_SLOTS - gap)
      }
      if (space > mostSpace) {
        mostSpace = space
        slot = candidate
      }
    }
    const light: ChasingLight = {
      x: point.x,
      y: point.y,
      id: this.nextId++,
      tier,
      slot,
      centerX: point.x,
      centerY: point.y,
      remaining: LIGHT_TIERS[tier].catchTime,
      trailX: 0,
      trailY: 0,
      opacity: 1,
    }
    if (this.anchor) {
      const offset = this.offset(light, this.anchor)
      // Start at the exact pickup point while the orbit's center begins catching up.
      light.centerX -= offset.x
      light.centerY -= offset.y
    }
    this.lights.push(light)
    return light
  }

  private move(light: ChasingLight, anchor: LightAnchor, elapsed: number, moving: boolean, settle: boolean) {
    const { response, catchTime, trailLength } = LIGHT_TIERS[light.tier]
    const oldX = light.centerX
    const oldY = light.centerY
    let blend = 1
    if (settle) {
      light.remaining = 0
    } else if (moving) {
      light.remaining = catchTime
      blend = -Math.expm1(-elapsed / response)
    } else if (light.remaining > elapsed) {
      // Ease in quickly after normal steps and bound the catch time even across the full screen.
      const remaining = light.remaining - elapsed
      blend = 1 - Math.exp(-5.5 * elapsed / response) * (remaining / light.remaining) ** 2
      light.remaining = remaining
    } else {
      light.remaining = 0
    }
    light.centerX += (anchor.x - oldX) * blend
    light.centerY += (anchor.y - oldY) * blend

    const { x, y } = this.offset(light, anchor)
    const margin = this.margin(light.tier, anchor.scale)
    light.x = fit(light.centerX + x, this.width, margin)
    light.y = fit(light.centerY + y, this.height, margin)
    if (settle || (!moving && light.remaining === 0)) {
      light.trailX = 0
      light.trailY = 0
      return
    }
    if (!elapsed) return
    const dx = oldX - light.centerX
    const dy = oldY - light.centerY
    const travel = Math.hypot(dx, dy)
    const maxLength = moving ? trailLength * anchor.scale : Math.hypot(light.trailX, light.trailY)
    const length = Math.min(maxLength, travel * (moving ? 0.11 : 0.055) / elapsed)
    const stretch = travel ? length / travel : 0
    const ease = -Math.expm1(-elapsed * (moving ? 15 : 28))
    light.trailX = fit(light.x + light.trailX + (dx * stretch - light.trailX) * ease, this.width, 0) - light.x
    light.trailY = fit(light.y + light.trailY + (dy * stretch - light.trailY) * ease, this.height, 0) - light.y
  }

  private offset(light: ChasingLight, anchor: LightAnchor) {
    const angle = this.phase + light.slot * CIRCLE / ORBIT_SLOTS
    const margin = this.margin(light.tier, anchor.scale)
    const gap = (LIGHT_TIERS[light.tier].orbitRadius + 5) * anchor.scale
    return {
      x: fitOrbit(anchor.x, anchor.radiusX + gap, this.width, margin, Math.cos(angle)) - anchor.x,
      y: fitOrbit(anchor.y, anchor.radiusY + gap, this.height, margin, Math.sin(angle)) - anchor.y,
    }
  }

  private margin(tier: LightTier, scale: number) {
    return LIGHT_TIERS[tier].orbitRadius * HALO_SCALE * scale + 1
  }

  private effect(kind: LightEffect['kind'], tier: LightTier, point: Point, inputs: ChasingLight[],
    delay: number, result: ChasingLight | null) {
    // A new event of the same kind and tier replaces the earlier transient effect.
    const existing = this.effects.findIndex((effect) => effect.kind === kind && effect.tier === tier)
    if (existing >= 0) {
      const [replaced] = this.effects.splice(existing, 1)
      if (replaced.result) replaced.result.opacity = 1
    }
    const effect: TimedEffect = {
      x: point.x,
      y: point.y,
      kind,
      tier,
      sources: inputs.map(({ x, y, tier }) => ({ x, y, tier })),
      inputs,
      result,
      progress: 0,
      elapsed: -delay,
      duration: kind === 'seal' ? SEAL_DURATION : MERGE_DURATION,
    }
    if (kind === 'seal') this.placeSeal(effect, point)
    this.effects.push(effect)
  }

  private placeSeal(effect: LightEffect, point: Point) {
    const margin = Math.min(38 * (this.anchor?.scale ?? 1), Math.min(this.width, this.height) * 0.18) + 2
    effect.x = fit(point.x, this.width, margin)
    effect.y = fit(point.y, this.height, margin)
  }
}
