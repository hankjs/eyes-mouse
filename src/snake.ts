export interface Point {
  x: number
  y: number
}

interface TrailPoint extends Point {
  distance: number
}

export interface Food extends Point {
  radius: number
  growth: number
  color: string
}

export type Gait = 'idle' | 'crawl' | 'walk' | 'jump'
export type Facing = -1 | 1

export const HEAD_RADIUS = 16
export const CHARACTER_INSET = 38
export const HOP_TAKEOFF = 0.11
export const HOP_LANDING = 0.42
export const HOP_DURATION = 0.54

const CRAWL_DISTANCE = 56
const WALK_DISTANCE = 64
const JUMP_DISTANCE = 220
const LAND_DISTANCE = 190
const START_LENGTH = 84
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
  gait: Gait = 'idle'
  gaitTime = 0
  facing: Facing = 1

  private length = START_LENGTH
  private target: { cursor: Point; destination: Point } | null = null
  private retryFoodAt = 0

  constructor(
    private readonly width: number,
    private readonly height: number,
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
      this.rest()
      return false
    }
    const insetX = Math.min(CHARACTER_INSET, this.width / 2)
    const insetY = Math.min(CHARACTER_INSET, this.height / 2)
    const destination = {
      x: clamp(point.x, insetX, this.width - insetX),
      y: clamp(point.y, insetY, this.height - insetY),
    }
    this.target = { cursor: point, destination }
    const moving = Math.hypot(destination.x - this.head.x, destination.y - this.head.y) > STOP_DISTANCE
    if (!moving) this.rest()
    return moving
  }

  advance(seconds: number): boolean {
    if (!this.target) return false
    const { cursor, destination } = this.target
    const dx = destination.x - this.head.x
    const dy = destination.y - this.head.y
    const distance = Math.hypot(dx, dy)
    if (distance <= STOP_DISTANCE) return false
    if (Math.abs(dx) > distance * 0.16) this.facing = dx < 0 ? -1 : 1

    this.updateGait(Math.hypot(cursor.x - this.head.x, cursor.y - this.head.y))
    const elapsed = clamp(seconds, 0, 0.05)
    let speed = this.gait === 'crawl' ? 150 : 310
    if (this.gait === 'jump') {
      const hop = this.gaitTime % HOP_DURATION
      speed = hop < HOP_TAKEOFF ? 160 : hop < HOP_LANDING ? 510 : 220
    }
    this.gaitTime += elapsed

    // Keep each move shorter than the contact radius, even after a delayed frame.
    let remaining = Math.min(distance, speed * elapsed)
    while (remaining > 0) {
      const step = Math.min(MAX_STEP, remaining)
      const next = {
        x: this.head.x + (dx / distance) * step,
        y: this.head.y + (dy / distance) * step,
        distance: this.head.distance + step,
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
    const moving = Math.hypot(destination.x - this.head.x, destination.y - this.head.y) > STOP_DISTANCE
    if (!moving) this.rest()
    return moving
  }

  private rest() {
    this.gait = 'idle'
    this.gaitTime = 0
  }

  private updateGait(gap: number) {
    if (this.gait === 'jump') {
      const hop = this.gaitTime % HOP_DURATION
      // Finish the current hop when the cursor gets closer, unless it is already at our feet.
      if (gap > LAND_DISTANCE || (gap > WALK_DISTANCE && hop < HOP_LANDING)) return
    }
    const crawlDistance = this.gait === 'crawl' ? WALK_DISTANCE : CRAWL_DISTANCE
    const nextGait = gap >= JUMP_DISTANCE ? 'jump' : gap <= crawlDistance ? 'crawl' : 'walk'
    if (nextGait !== this.gait) {
      this.gait = nextGait
      this.gaitTime = 0
    }
  }

  private nearBody(point: Point, radius: number) {
    for (let i = 1; i < this.trail.length; i++) {
      const a = this.trail[i - 1]
      const b = this.trail[i]
      const dx = b.x - a.x
      const dy = b.y - a.y
      const fraction = clamp(
        ((point.x - a.x) * dx + (point.y - a.y) * dy) / (dx * dx + dy * dy),
        0,
        1,
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
    const clearance = radius + FOOD_GAP
    const inset = Math.max(CHARACTER_INSET, clearance)
    const width = this.width - 2 * inset
    const height = this.height - 2 * inset
    if (width <= 0 || height <= 0) return null
    const open = (point: Point) => {
      if (this.food.some((pellet) => within(point, pellet, clearance + pellet.radius))) return false
      const bodyClearance = clearance + HEAD_RADIUS
      return !within(point, this.head, bodyClearance) && !this.nearBody(point, bodyClearance)
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
