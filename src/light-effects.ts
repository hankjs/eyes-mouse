import { LIGHT_TIERS, type LightEffect, type LightOrbit, type OrbitLight } from './lights'

export type PaintBounds = [number, number, number, number]

const TURN = 2 * Math.PI

function bounds(x: number, y: number, radius: number, trailX = 0, trailY = 0): PaintBounds {
  const margin = radius + 2
  const left = Math.floor(x + Math.min(0, trailX) - margin)
  const top = Math.floor(y + Math.min(0, trailY) - margin)
  const right = Math.ceil(x + Math.max(0, trailX) + margin)
  const bottom = Math.ceil(y + Math.max(0, trailY) + margin)
  return [left, top, right - left, bottom - top]
}

function drawTrail(ctx: CanvasRenderingContext2D, x: number, y: number, trailX: number, trailY: number, radius: number) {
  const length = Math.hypot(trailX, trailY)
  if (length < 0.5) return
  const tailX = x + trailX
  const tailY = y + trailY
  const bend = Math.min(0.11, 0.6 * radius / length)
  const gradient = ctx.createLinearGradient(tailX, tailY, x, y)
  gradient.addColorStop(0, 'rgba(200, 105, 11, 0)')
  gradient.addColorStop(0.5, 'rgba(213, 130, 21, 0.40)')
  gradient.addColorStop(1, 'rgba(244, 163, 30, 0.65)')
  ctx.beginPath()
  ctx.moveTo(tailX, tailY)
  ctx.quadraticCurveTo(x + 0.42 * trailX - trailY * bend, y + 0.42 * trailY + trailX * bend, x, y)
  ctx.lineCap = 'round'
  ctx.strokeStyle = gradient
  ctx.lineWidth = Math.max(1, 0.52 * radius)
  ctx.stroke()

  const glint = ctx.createLinearGradient(tailX, tailY, x, y)
  glint.addColorStop(0, 'rgba(255, 206, 71, 0)')
  glint.addColorStop(0.5, 'rgba(255, 208, 75, 0.35)')
  glint.addColorStop(1, 'rgba(255, 234, 147, 0.80)')
  ctx.strokeStyle = glint
  ctx.lineWidth = Math.max(0.5, 0.21 * radius)
  ctx.stroke()
}

function drawLight(ctx: CanvasRenderingContext2D, light: Omit<OrbitLight, 'id'>, scale: number, twinkle: number) {
  const { x, y, tier, opacity, trailX, trailY } = light
  const radius = LIGHT_TIERS[tier].orbitRadius * scale
  const haloRadius = 1.7 * radius
  const ringRadius = 0.72 * radius
  ctx.globalAlpha = opacity
  drawTrail(ctx, x, y, trailX, trailY, radius)

  const halo = ctx.createRadialGradient(x, y, ringRadius * 0.48, x, y, haloRadius)
  halo.addColorStop(0, 'rgba(255, 210, 83, 0.55)')
  halo.addColorStop(0.50, 'rgba(240, 158, 29, 0.24)')
  halo.addColorStop(1, 'rgba(193, 97, 14, 0)')
  ctx.beginPath()
  ctx.arc(x, y, haloRadius, 0, TURN)
  ctx.fillStyle = halo
  ctx.globalAlpha = opacity * (0.76 + 0.24 * twinkle)
  ctx.fill()

  const core = ctx.createRadialGradient(x, y, 0, x, y, ringRadius)
  core.addColorStop(0, '#ffffec')
  core.addColorStop(0.32, '#fff4ab')
  core.addColorStop(0.64, '#ffce43')
  core.addColorStop(1, '#e89b14')
  ctx.beginPath()
  ctx.arc(x, y, ringRadius, 0, TURN)
  ctx.globalAlpha = opacity
  ctx.fillStyle = core
  ctx.fill()
  ctx.strokeStyle = '#b5741d'
  ctx.lineWidth = Math.max(0.7, 0.11 * radius)
  ctx.stroke()

  const point = radius * (0.71 + 0.13 * twinkle + 0.06 * tier)
  const waist = 0.17 * radius
  ctx.beginPath()
  ctx.moveTo(x, y - point)
  ctx.lineTo(x + waist, y - waist)
  ctx.lineTo(x + 0.72 * point, y)
  ctx.lineTo(x + waist, y + waist)
  ctx.lineTo(x, y + point)
  ctx.lineTo(x - waist, y + waist)
  ctx.lineTo(x - 0.72 * point, y)
  ctx.lineTo(x - waist, y - waist)
  ctx.closePath()
  ctx.fillStyle = '#fff8d6'
  ctx.fill()
  return bounds(x, y, haloRadius, trailX, trailY)
}

function drawGather(ctx: CanvasRenderingContext2D, effect: LightEffect, scale: number, progress: number) {
  const reach = progress * progress * (3 - 2 * progress)
  const earlier = Math.max(0, progress - 0.1)
  const tailReach = earlier * earlier * (3 - 2 * earlier)
  const opacity = 1 - progress ** 2
  return effect.sources.map(({ x, y, tier }) => {
    const dx = effect.x - x
    const dy = effect.y - y
    const distance = Math.hypot(dx, dy)
    const trail = distance ? Math.min(reach - tailReach, 12 * scale / distance) : 0
    return drawLight(ctx, {
      x: x + dx * reach, y: y + dy * reach, tier, opacity,
      trailX: -dx * trail, trailY: -dy * trail,
    }, scale * (1 - 0.42 * progress), 0.6)
  })
}

function drawSeal(ctx: CanvasRenderingContext2D, effect: LightEffect, scale: number, sealLimit: number, reduced: boolean) {
  const { x, y, progress } = effect
  const opening = reduced ? 0.69 : 0.25 + 0.75 * (1 - (1 - progress) ** 3)
  const radius = Math.min(38 * scale, sealLimit) * opening
  const opacity = reduced ? 0.58 * (1 - progress) : Math.min(1, 0.34 + 6 * progress) * (1 - progress) ** 0.92
  const rotation = reduced ? 0 : progress * 0.28
  const line = Math.max(0.65, radius * 0.018)
  ctx.globalAlpha = opacity
  ctx.lineCap = 'round'
  ctx.lineJoin = 'round'
  ctx.beginPath()
  ctx.arc(x, y, radius, 0, TURN)
  ctx.moveTo(x + radius * 0.73, y)
  ctx.arc(x, y, radius * 0.73, 0, TURN)
  for (let index = 0; index < 16; index++) {
    const angle = rotation + index * TURN / 16
    const cos = Math.cos(angle)
    const sin = Math.sin(angle)
    const cardinal = index % 4 === 0
    const inner = radius * (cardinal ? 0.79 : 0.91)
    const outer = radius * 0.96
    if (cardinal) {
      const middle = (inner + outer) / 2
      const corner = radius * 0.037
      ctx.moveTo(x + inner * cos, y + inner * sin)
      ctx.lineTo(x + middle * cos - corner * sin, y + middle * sin + corner * cos)
      ctx.lineTo(x + outer * cos, y + outer * sin)
      ctx.lineTo(x + middle * cos + corner * sin, y + middle * sin - corner * cos)
      ctx.closePath()
    } else {
      ctx.moveTo(x + inner * cos, y + inner * sin)
      ctx.lineTo(x + outer * cos, y + outer * sin)
    }
  }
  ctx.strokeStyle = 'rgba(158, 81, 9, 0.75)'
  ctx.lineWidth = 2.55 * line
  ctx.stroke()
  ctx.strokeStyle = '#ffe29a'
  ctx.lineWidth = line
  ctx.stroke()

  ctx.beginPath()
  const band = radius * 0.55
  for (let index = 0; index < 4; index++) {
    const from = rotation + 0.23 + index * TURN / 4
    ctx.moveTo(x + band * Math.cos(from), y + band * Math.sin(from))
    ctx.arc(x, y, band, from, from + 0.94)
  }
  ctx.strokeStyle = 'rgba(240, 166, 39, 0.80)'
  ctx.lineWidth = line
  ctx.stroke()
  return bounds(x, y, radius + 1.3 * line)
}

export function drawLightEffects(ctx: CanvasRenderingContext2D, orbit: LightOrbit, time: number, reduced: boolean, sealLimit: number) {
  const scale = orbit.anchor?.scale ?? 1
  const painted: PaintBounds[] = []
  ctx.save()
  for (const effect of orbit.effects) {
    if (effect.progress <= 0) continue
    if (effect.kind === 'seal') {
      painted.push(drawSeal(ctx, effect, scale, sealLimit, reduced))
    }
    if (!reduced && (effect.kind === 'merge' || effect.progress < 0.33)) {
      const progress = effect.kind === 'merge' ? effect.progress : effect.progress / 0.33
      painted.push(...drawGather(ctx, effect, scale, progress))
    }
  }
  for (const light of [...orbit.lights, ...orbit.waitingLights]) {
    if (light.opacity <= 0) continue
    const twinkle = reduced ? 0.5 : 0.5 + 0.5 * Math.sin(time * 3.1 + 2.4 * light.id)
    painted.push(drawLight(ctx, light, scale, twinkle))
  }
  ctx.restore()
  return painted
}
