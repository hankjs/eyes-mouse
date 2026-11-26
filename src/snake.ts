export interface Point {
  x: number
  y: number
}

interface TrailPoint extends Point {
  distance: number
}

export interface Bounds {
  left: number
  top: number
  right: number
  bottom: number
}

export interface Food extends Point {
  radius: number
  growth: number
  color: string
}

export const HEAD_RADIUS = 10
export const BODY_RADIUS = 7

const START_LENGTH = 84
const NECK_LENGTH = 34
const SPEED = 540
const MAX_STEP = 4
const STOP_DISTANCE = 1
const FOOD_GAP = 8
const FOOD_SIZES = [
  { radius: 7, growth: 24, color: '#ffc16f' },
  { radius: 10, growth: 36, color: '#ff8a72' },
  { radius: 14, growth: 52, color: '#f59ed4' },
]

function clamp(value: number, min: number, max: number) {
  return Math.max(min, Math.min(max, value))
}

function within(a: Point, b: Point, radius: number) {
  return (a.x - b.x) ** 2 + (a.y - b.y) ** 2 < radius ** 2
}

export class SnakeGame {
  readonly trail: TrailPoint[]
  readonly food: Food[] = []
  score = 0
  heading = 0
  crashed = false

  private length = START_LENGTH
  private target: Point | null = null
  private retryFoodAt = 0

  constructor(
    private readonly width: number,
    private readonly height: number,
    private readonly controls: Bounds,
    private readonly random = Math.random,
  ) {
    this.trail = [{ x: width / 2, y: height / 2, distance: 0 }]
    this.spawnBatch()
  }

  get head() {
    return this.trail[this.trail.length - 1]
  }

  setTarget(point: Point | null) {
    if (!point) {
      this.target = null
      return
    }
    const insetX = Math.min(HEAD_RADIUS, this.width / 2)
    const insetY = Math.min(HEAD_RADIUS, this.height / 2)
    this.target = {
      x: clamp(point.x, insetX, this.width - insetX),
      y: clamp(point.y, insetY, this.height - insetY),
    }
  }

  advance(seconds: number): boolean {
    if (!this.target || this.crashed) return false
    let remaining = SPEED * clamp(seconds, 0, 0.05)
    const dx = this.target.x - this.head.x
    const dy = this.target.y - this.head.y
    const distance = Math.hypot(dx, dy)
    if (distance <= STOP_DISTANCE) return false
    this.heading = Math.atan2(dy, dx)

    // Keep each move shorter than the contact radius, even after a delayed frame.
    remaining = Math.min(distance, remaining)
    while (remaining > 0) {
      const step = Math.min(MAX_STEP, remaining)
      const next = {
        x: this.head.x + (dx / distance) * step,
        y: this.head.y + (dy / distance) * step,
        distance: this.head.distance + step,
      }
      const tail = next.distance - this.length
      if (this.nearBody(next, HEAD_RADIUS + BODY_RADIUS, tail, next.distance - NECK_LENGTH)) {
        this.crashed = true
        return false
      }

      this.trail.push(next)
      const bite = this.food.findIndex((pellet) => within(next, pellet, HEAD_RADIUS + pellet.radius))
      if (bite >= 0) {
        const [pellet] = this.food.splice(bite, 1)
        this.length += pellet.growth
        this.score++
      }
      this.trimTail()
      if (!this.food.length && (bite >= 0 || next.distance >= this.retryFoodAt)) {
        this.spawnBatch()
      }
      remaining -= step
    }
    return Math.hypot(this.target.x - this.head.x, this.target.y - this.head.y) > STOP_DISTANCE
  }

  private nearBody(point: Point, radius: number, start: number, end: number) {
    if (start > end) return false
    for (let i = 1; i < this.trail.length; i++) {
      const a = this.trail[i - 1]
      const b = this.trail[i]
      if (b.distance < start) continue
      if (a.distance > end) break
      const dx = b.x - a.x
      const dy = b.y - a.y
      const length = b.distance - a.distance
      const fraction = clamp(
        ((point.x - a.x) * dx + (point.y - a.y) * dy) / (dx * dx + dy * dy),
        Math.max(0, (start - a.distance) / length),
        Math.min(1, (end - a.distance) / length),
      )
      if (within(point, { x: a.x + dx * fraction, y: a.y + dy * fraction }, radius)) return true
    }
    return false
  }

  private trimTail() {
    const trimAt = this.head.distance - this.length
    let trimCount = 0
    while (trimCount < this.trail.length - 2 && this.trail[trimCount + 1].distance <= trimAt) {
      trimCount++
    }
    if (trimCount) this.trail.splice(0, trimCount)
    const [tail, next] = this.trail
    if (trimAt <= tail.distance) return
    const fraction = (trimAt - tail.distance) / (next.distance - tail.distance)
    this.trail[0] = {
      x: tail.x + (next.x - tail.x) * fraction,
      y: tail.y + (next.y - tail.y) * fraction,
      distance: trimAt,
    }
  }

  private spawnBatch() {
    const sizes = [...FOOD_SIZES]
    const count = 1 + Math.floor(this.random() * sizes.length)
    while (this.food.length < count && sizes.length) {
      const [size] = sizes.splice(Math.floor(this.random() * sizes.length), 1)
      const point = this.openSpot(size.radius)
      if (point) this.food.push({ ...size, ...point })
    }
    this.retryFoodAt = this.head.distance + 48
  }

  private openSpot(radius: number): Point | null {
    const inset = radius + FOOD_GAP
    const width = this.width - 2 * inset
    const height = this.height - 2 * inset
    if (width <= 0 || height <= 0) return null
    const open = (point: Point) => {
      const { left, right, bottom, top } = this.controls
      if (point.x > left - inset && point.x < right + inset && point.y > top - inset && point.y < bottom + inset) {
        return false
      }
      if (this.food.some((pellet) => within(point, pellet, inset + pellet.radius))) return false
      const clearance = inset + HEAD_RADIUS
      return !within(point, this.head, clearance)
        && !this.nearBody(point, clearance, this.trail[0].distance, this.head.distance)
    }
    for (let attempt = 0; attempt < 32; attempt++) {
      const point = { x: inset + this.random() * width, y: inset + this.random() * height }
      if (open(point)) return point
    }
    for (let y = 0; y <= 6; y++) {
      for (let x = 0; x <= 6; x++) {
        const point = { x: inset + (x / 6) * width, y: inset + (y / 6) * height }
        if (open(point)) return point
      }
    }
    return null
  }
}
