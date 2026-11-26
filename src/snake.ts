import { BeadTail } from './tail.ts'

export interface Point {
  x: number
  y: number
}

export interface Food extends Point {
  radius: number
  color: string
}

export type Gait = 'idle' | 'crawl' | 'walk' | 'jump'
export type Facing = -1 | 1

export const HEAD_RADIUS = 16
export const CHARACTER_INSET = 38
export const SNAKE_SIZE_MIN = 100
export const SNAKE_SIZE_MAX = 300
export const SNAKE_SIZE_STEP = 25
export const DEFAULT_SNAKE_SIZE = 200
export const HOP_TAKEOFF = 0.11
export const HOP_LANDING = 0.42
export const HOP_DURATION = 0.54

const MAX_HOPS = 4
const NOMINAL_HOP_WIDTH = 0.34
const WARMUP_SHARE = 0.12
const MAX_STEP = 4
const STOP_DISTANCE = 1
const FOOD_GAP = 8
const FOOD_SIZES = [
  { radius: 7, color: '#ffc16f' },
  { radius: 10, color: '#ff8a72' },
  { radius: 14, color: '#f59ed4' },
]

function clamp(value: number, min: number, max: number) {
  return Math.max(min, Math.min(max, value))
}

function within(a: Point, b: Point, radius: number) {
  return (a.x - b.x) ** 2 + (a.y - b.y) ** 2 < radius ** 2
}

function hopProgress(time: number) {
  const completed = Math.floor(time / HOP_DURATION)
  const phase = time % HOP_DURATION
  const takeoff = clamp(phase / HOP_TAKEOFF, 0, 1)
  const flight = clamp((phase - HOP_TAKEOFF) / (HOP_LANDING - HOP_TAKEOFF), 0, 1)
  return completed + WARMUP_SHARE * takeoff + (1 - WARMUP_SHARE) * flight
}

export class SnakeGame {
  readonly tail: BeadTail
  readonly food: Food[] = []
  head: Point
  score = 0
  gait: Gait = 'idle'
  gaitTime = 0
  facing: Facing = 1

  private distanceTravelled = 0
  private target: Point | null = null
  private hopCount = 0
  private hopAim: Point | null = null
  private retryFoodAt = 0

  constructor(
    private readonly width: number,
    private readonly height: number,
    private readonly random = Math.random,
    private readonly displayWidth = width,
  ) {
    this.head = { x: width / 2, y: height / 2 }
    this.tail = new BeadTail(width, height)
    this.spawnBatch()
  }

  setTarget(point: Point | null) {
    if (!point) {
      this.target = null
      this.rest()
      return false
    }
    const insetX = Math.min(CHARACTER_INSET, this.width / 2)
    const insetY = Math.min(CHARACTER_INSET, this.height / 2)
    this.target = {
      x: clamp(point.x, insetX, this.width - insetX),
      y: clamp(point.y, insetY, this.height - insetY),
    }
    const moving = Math.hypot(this.target.x - this.head.x, this.target.y - this.head.y) > STOP_DISTANCE
    if (!moving && this.gait !== 'jump') this.rest()
    return moving
  }

  advance(seconds: number): boolean {
    const destination = this.target
    if (!destination) return false
    const dx = destination.x - this.head.x
    const dy = destination.y - this.head.y
    const distance = Math.hypot(dx, dy)
    if (Math.abs(dx) > Math.max(STOP_DISTANCE * 4, distance * 0.16)) this.facing = dx < 0 ? -1 : 1
    if (distance > STOP_DISTANCE) this.updateGait(distance, destination)
    if (this.gait === 'idle') return false
    const elapsed = clamp(seconds, 0, 0.05)
    const previousTime = this.gaitTime
    this.gaitTime += elapsed

    // Keep each move shorter than the contact radius, even after a delayed frame.
    let remaining = 0
    if (distance > STOP_DISTANCE) {
      if (this.gait === 'jump') {
        remaining = this.hopDistance(distance, previousTime)
      } else {
        // At the gait limits, allow half a second of walking and one second of crawling.
        const speed = this.gait === 'crawl'
          ? Math.max(150, this.displayWidth / 5)
          : Math.max(310, this.displayWidth * 2 / 5)
        remaining = Math.min(distance, elapsed * speed)
      }
    }
    while (remaining > 0) {
      const step = Math.min(MAX_STEP, remaining)
      const next = {
        x: this.head.x + (dx / distance) * step,
        y: this.head.y + (dy / distance) * step,
      }
      this.head = next
      this.distanceTravelled += step
      const bite = this.food.findIndex((pellet) => within(next, pellet, HEAD_RADIUS + pellet.radius))
      if (bite >= 0) {
        const [pellet] = this.food.splice(bite, 1)
        this.tail.add(pellet)
        this.score++
      }
      if (!this.food.length && (bite >= 0 || this.distanceTravelled >= this.retryFoodAt)) {
        this.spawnBatch()
      }
      remaining -= step
    }
    const gap = Math.hypot(destination.x - this.head.x, destination.y - this.head.y)
    if (gap <= STOP_DISTANCE) {
      const finishedHop = Math.floor(this.gaitTime / HOP_DURATION) > Math.floor(previousTime / HOP_DURATION)
      if (this.gait === 'jump' && !finishedHop) return true
      this.rest()
      return false
    }
    return true
  }

  private rest() {
    this.gait = 'idle'
    this.gaitTime = 0
  }

  private updateGait(gap: number, destination: Point) {
    const jumping = this.gait === 'jump'
    const walkLimit = this.displayWidth * 2 / 5
    if (jumping && this.gaitTime < this.hopCount * HOP_DURATION) {
      if (this.hopAim && Math.hypot(destination.x - this.hopAim.x, destination.y - this.hopAim.y) > this.displayWidth * 0.1) {
        this.hopAim = destination
        if (gap <= walkLimit) this.hopCount = Math.floor(this.gaitTime / HOP_DURATION) + 1
        else this.planHops(gap)
      }
      return
    }
    const nextGait = gap <= this.displayWidth / 5 ? 'crawl' : gap <= walkLimit ? 'walk' : 'jump'
    if (nextGait === this.gait && !jumping) return
    this.gait = nextGait
    this.gaitTime = 0
    if (nextGait === 'jump') {
      this.hopAim = destination
      this.planHops(gap)
    }
  }

  private planHops(gap: number) {
    // An airborne hop still counts toward a new target's four landings.
    const progress = hopProgress(this.gaitTime)
    this.hopCount = Math.min(Math.floor(progress) + MAX_HOPS, Math.ceil(progress + gap / (this.displayWidth * NOMINAL_HOP_WIDTH)))
  }

  private hopDistance(distance: number, previousTime: number) {
    const progress = hopProgress(previousTime)
    const budget = this.hopCount - progress
    if (budget <= 0) return 0
    const next = Math.min(this.hopCount, hopProgress(this.gaitTime))
    const frameShare = Math.max(0, next - progress)
    // Allow a whole-screen retarget even if the first counted hop was almost over.
    const maxHop = Math.max(this.displayWidth * NOMINAL_HOP_WIDTH, Math.hypot(this.width, this.height) / (MAX_HOPS - 1))
    return Math.min(distance * frameShare / budget, maxHop * frameShare)
  }

  private spawnBatch() {
    const sizes = [...FOOD_SIZES]
    const count = 1 + Math.floor(this.random() * sizes.length)
    while (this.food.length < count && sizes.length) {
      const [size] = sizes.splice(Math.floor(this.random() * sizes.length), 1)
      const point = this.openSpot(size.radius)
      if (point) this.food.push({ ...size, ...point })
    }
    this.retryFoodAt = this.distanceTravelled + 48
  }

  private openSpot(radius: number): Point | null {
    const clearance = radius + FOOD_GAP
    const inset = Math.max(CHARACTER_INSET, clearance)
    const width = this.width - 2 * inset
    const height = this.height - 2 * inset
    if (width <= 0 || height <= 0) return null
    const open = (point: Point) => {
      if (this.food.some((pellet) => within(point, pellet, clearance + pellet.radius))) return false
      return !within(point, this.head, clearance + HEAD_RADIUS) && !this.tail.overlaps(point, clearance)
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
