import type { Food, Point } from './snake.ts'

export interface TailAnchor extends Point {
  angle: number
  scale: number
}

interface Bead extends Food {
  angle: number
  velocity: number
  length: number
}

const DOWN = Math.PI / 2
const MAX_FRAME = 0.05
const TIME_STEP = 1 / 120
const SETTLE_TIME = 0.72
const SPRING = 240
const DAMPING = 19
export const BEAD_OUTLINE = 0.9

function turn(from: number, to: number) {
  const difference = to - from
  return Math.atan2(Math.sin(difference), Math.cos(difference))
}

export class BeadTail {
  readonly beads: Bead[] = []
  anchor: TailAnchor | null = null

  private settling = false
  private quietTime = 0

  constructor(private readonly width: number, private readonly height: number) {
    this.append(4.9, '#949dc7')
    this.append(4.3, '#b0bae0')
  }

  add(food: Food) {
    this.append(3 + food.radius * 0.28, food.color)
  }

  overlaps(point: Point, clearance: number) {
    const scale = this.anchor?.scale
    return scale !== undefined && this.beads.some((bead) =>
      Math.hypot(point.x - bead.x, point.y - bead.y) < clearance + bead.radius * scale,
    )
  }

  follow(anchor: TailAnchor, seconds: number, snap = false): boolean {
    const last = this.anchor
    this.anchor = anchor
    if (snap || !last || anchor.scale !== last.scale) {
      this.layout(anchor)
      this.quietTime = 0
      this.settling = false
      return false
    }

    const drift = Math.hypot(anchor.x - last.x, anchor.y - last.y)
    const rotation = Math.abs(turn(last.angle, anchor.angle))
    if (!this.settling && drift === 0 && rotation === 0) return false
    const elapsed = Math.min(MAX_FRAME, Math.max(0, seconds))
    this.quietTime = drift > 0.5 || rotation > 0.01 ? 0 : this.quietTime + elapsed
    const steps = Math.max(1, Math.ceil(elapsed / TIME_STEP))
    let unsettled = false
    for (let step = 0; step < steps; step++) unsettled = this.layout(anchor, elapsed / steps)
    this.settling = unsettled && this.quietTime < SETTLE_TIME
    // Keep the edge the moving links settled against, even if their ideal angles fit elsewhere.
    if (!this.settling) for (const bead of this.beads) bead.velocity = 0
    return this.settling
  }

  private append(radius: number, color: string) {
    const previous = this.beads.at(-1)
    this.beads.push({
      x: previous?.x ?? 0,
      y: previous?.y ?? 0,
      angle: previous?.angle ?? 0,
      velocity: previous?.velocity ?? 0,
      length: radius + (previous ? previous.radius + 1.4 : 3.3),
      radius,
      color,
    })
    this.quietTime = 0
    this.settling = true
  }

  private layout(anchor: TailAnchor, step?: number) {
    let joint: Point = anchor
    let previousAngle = anchor.angle
    let unsettled = false
    for (const bead of this.beads) {
      const length = bead.length * anchor.scale
      const margin = (bead.radius + BEAD_OUTLINE) * anchor.scale
      // Each link follows its parent and keeps some pull toward the base of the tail.
      const preferred = joint === anchor ? anchor.angle : previousAngle
        + 0.24 * turn(previousAngle, anchor.angle) + 0.16 * turn(previousAngle, DOWN)
      const target = this.fitAngle(joint, preferred, length, margin)
      const error = turn(bead.angle, target)
      if (step !== undefined) {
        bead.velocity += (SPRING * error - DAMPING * bead.velocity) * step
        const next = bead.angle + bead.velocity * step
        bead.angle = this.fitAngle(joint, next, length, margin)
        if (Math.abs(turn(next, bead.angle)) > 0.0001) bead.velocity = 0
        if (Math.abs(error) > 0.02 || Math.abs(bead.velocity) > 0.12) unsettled = true
      } else {
        bead.angle = target
        bead.velocity = 0
      }
      bead.x = joint.x + length * Math.cos(bead.angle)
      bead.y = joint.y + length * Math.sin(bead.angle)
      previousAngle = bead.angle
      joint = bead
    }
    return unsettled
  }

  private fitAngle(joint: Point, angle: number, length: number, margin: number) {
    const fits = (bearing: number) => {
      const x = joint.x + length * Math.cos(bearing)
      const y = joint.y + length * Math.sin(bearing)
      return x >= margin - 0.0001 && x <= this.width - margin + 0.0001
        && y >= margin - 0.0001 && y <= this.height - margin + 0.0001
    }
    if (fits(angle)) return angle

    // Keep the link length by finding the closest point where its circle meets a screen edge.
    let closest = angle
    let leastTurn = Infinity
    const consider = (candidate: number) => {
      const difference = Math.abs(turn(angle, candidate))
      if (difference >= leastTurn || !fits(candidate)) return
      leastTurn = difference
      closest = candidate
    }
    for (const x of [margin, this.width - margin]) {
      const cosine = (x - joint.x) / length
      if (Math.abs(cosine) > 1) continue
      const candidate = Math.acos(cosine)
      consider(candidate)
      consider(-candidate)
    }
    for (const y of [margin, this.height - margin]) {
      const sine = (y - joint.y) / length
      if (Math.abs(sine) > 1) continue
      const candidate = Math.asin(sine)
      consider(candidate)
      consider(Math.PI - candidate)
    }
    return closest
  }
}
