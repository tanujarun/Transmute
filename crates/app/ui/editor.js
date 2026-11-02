// One page for every editor tool. Each entry in TOOLS builds its controls and
// returns a function producing the edit sent to the backend.
const { invoke, convertFileSrc } = window.__TAURI__.core;
const FRAME = 1 / 30;
const $ = id => document.getElementById(id);
let info = null;
let player = null;

const fmt = seconds => Number(seconds).toFixed(3);
const panel = $('panel');
const overlay = $('overlay');

function row(html) {
  const element = document.createElement('div');
  element.className = 'row';
  element.innerHTML = html;
  panel.append(element);
  return element;
}

function numberInput(id, label, value, step = 0.001) {
  return `<label>${label} <input id="${id}" type="number" min="0" step="${step}" value="${value}"></label>`;
}

function value(id) {
  return Number($(id).value);
}

function setFromPlayhead(button, input) {
  $(button).onclick = () => { $(input).value = fmt(player.currentTime); };
}

function rangeRow() {
  row(`${numberInput('start', 'Start', 0)}<button id="setStart">Use playhead</button>
       ${numberInput('end', 'End', fmt(info.duration))}<button id="setEnd">Use playhead</button>`);
  setFromPlayhead('setStart', 'start');
  setFromPlayhead('setEnd', 'end');
}

/// A list with add and remove, kept in `items`.
function listRow(items, describe) {
  const list = document.createElement('ul');
  panel.append(list);
  const render = () => {
    list.replaceChildren(...items.map((item, index) => {
      const line = document.createElement('li');
      line.textContent = `${describe(item)} `;
      const remove = document.createElement('button');
      remove.textContent = 'Remove';
      remove.onclick = () => { items.splice(index, 1); render(); };
      line.append(remove);
      return line;
    }));
  };
  return render;
}

/// Lets the user drag a rectangle over the video, in source pixels.
function drawRectangles(onRectangle) {
  overlay.style.display = 'block';
  let origin = null;
  let box = null;
  const point = event => {
    const bounds = overlay.getBoundingClientRect();
    return {
      x: Math.min(Math.max(event.clientX - bounds.left, 0), bounds.width),
      y: Math.min(Math.max(event.clientY - bounds.top, 0), bounds.height),
    };
  };
  overlay.onmousedown = event => {
    origin = point(event);
    box = document.createElement('div');
    box.className = 'box';
    overlay.append(box);
  };
  overlay.onmousemove = event => {
    if (!origin) return;
    const now = point(event);
    box.style.left = `${Math.min(origin.x, now.x)}px`;
    box.style.top = `${Math.min(origin.y, now.y)}px`;
    box.style.width = `${Math.abs(now.x - origin.x)}px`;
    box.style.height = `${Math.abs(now.y - origin.y)}px`;
  };
  window.onmouseup = event => {
    if (!origin) return;
    const now = point(event);
    const scale = info.width / overlay.getBoundingClientRect().width;
    const rectangle = {
      x: Math.round(Math.min(origin.x, now.x) * scale),
      y: Math.round(Math.min(origin.y, now.y) * scale),
      width: Math.round(Math.abs(now.x - origin.x) * scale),
      height: Math.round(Math.abs(now.y - origin.y) * scale),
    };
    origin = null;
    if (rectangle.width > 3 && rectangle.height > 3) onRectangle(rectangle, box);
    else box.remove();
  };
}

/// Adds buttons that find faces or text and hand back rectangles in source pixels.
function detectRow(onFound, secondsNow) {
  row('<button id="findFaces">Find faces</button><button id="findText">Find text</button><span id="found"></span>');
  const find = key => async () => {
    $('found').textContent = 'Looking…';
    try {
      const result = await invoke('editor_detect', { seconds: secondsNow() });
      result[key].forEach(([x, y, width, height]) => onFound({ x, y, width, height }));
      $('found').textContent = `Found ${result[key].length}`;
    } catch (error) {
      $('found').textContent = String(error);
    }
  };
  $('findFaces').onclick = find('faces');
  $('findText').onclick = find('text');
}

/// Crop controls shared by video and photos.
function cropPanel(tool) {
  row(`${numberInput('x', 'X', 0, 1)}${numberInput('y', 'Y', 0, 1)}
       ${numberInput('w', 'Width', info.width, 1)}${numberInput('h', 'Height', info.height, 1)}`);
  row(`Aspect: <button data-ratio="0">Free</button><button data-ratio="1">1:1</button>
       <button data-ratio="1.7778">16:9</button><button data-ratio="0.5625">9:16</button>`);
  panel.querySelectorAll('[data-ratio]').forEach(button => {
    button.onclick = () => {
      const ratio = Number(button.dataset.ratio);
      if (!ratio) return;
      const width = Math.min(info.width, Math.round(info.height * ratio));
      $('w').value = width;
      $('h').value = Math.round(width / ratio);
    };
  });
  drawRectangles((rectangle, box) => {
    [...overlay.children].filter(child => child !== box).forEach(child => child.remove());
    $('x').value = rectangle.x;
    $('y').value = rectangle.y;
    $('w').value = rectangle.width;
    $('h').value = rectangle.height;
  });
  return () => ({ tool, x: value('x'), y: value('y'), width: value('w'), height: value('h') });
}

function slider(id, label, min, max, step, start) {
  return `<label>${label} <input id="${id}" type="range" min="${min}" max="${max}" step="${step}" value="${start}"></label>`;
}

const SVG = 'http://www.w3.org/2000/svg';
function svgElement(name, attributes) {
  const element = document.createElementNS(SVG, name);
  Object.entries(attributes).forEach(([key, val]) => element.setAttribute(key, val));
  return element;
}

/// Draws the marks in source pixels; the viewBox scales them to the preview.
function renderShapes(svg, shapes) {
  const stroke = Math.max(Math.floor(Math.max(info.width, info.height) / 300), 2);
  svg.replaceChildren(...shapes.flatMap(shape => {
    const line = { stroke: shape.color, 'stroke-width': stroke, fill: 'none' };
    const left = Math.min(shape.x1, shape.x2);
    const top = Math.min(shape.y1, shape.y2);
    const width = Math.abs(shape.x2 - shape.x1);
    const height = Math.abs(shape.y2 - shape.y1);
    if (shape.kind === 'arrow') {
      const angle = Math.atan2(shape.y2 - shape.y1, shape.x2 - shape.x1);
      const head = stroke * 4;
      const tip = (offset) => `${shape.x2 - head * Math.cos(angle + offset)},${shape.y2 - head * Math.sin(angle + offset)}`;
      return [
        svgElement('line', { ...line, x1: shape.x1, y1: shape.y1, x2: shape.x2, y2: shape.y2 }),
        svgElement('polygon', { fill: shape.color, points: `${shape.x2},${shape.y2} ${tip(-0.45)} ${tip(0.45)}` }),
      ];
    }
    if (shape.kind === 'box') return [svgElement('rect', { ...line, x: left, y: top, width, height })];
    if (shape.kind === 'highlight') return [svgElement('rect', { fill: '#ffe600', opacity: 0.4, x: left, y: top, width, height })];
    if (shape.kind === 'ellipse') {
      return [svgElement('ellipse', { ...line, cx: left + width / 2, cy: top + height / 2, rx: width / 2, ry: height / 2 })];
    }
    if (shape.kind === 'text') {
      const text = svgElement('text', { x: shape.x, y: shape.y, fill: shape.color, 'font-size': shape.size, 'font-family': 'Arial' });
      text.textContent = shape.text;
      return [text];
    }
    const label = svgElement('text', {
      x: shape.x, y: shape.y + shape.size * 0.36, fill: 'white', 'font-size': shape.size, 'font-family': 'Arial', 'text-anchor': 'middle',
    });
    label.textContent = shape.value;
    return [svgElement('circle', { cx: shape.x, cy: shape.y, r: shape.size * 0.75, fill: shape.color }), label];
  }));
}

const TOOLS = {
  'crop-image'() {
    return cropPanel('crop-image');
  },
  'resize-image'() {
    row(`${numberInput('w', 'Width', info.width, 1)}${numberInput('h', 'Height', info.height, 1)}
         <label><input id="lock" type="checkbox" checked> Keep proportions</label>`);
    $('w').oninput = () => {
      if ($('lock').checked) $('h').value = Math.max(1, Math.round(value('w') * info.height / info.width));
    };
    $('h').oninput = () => {
      if ($('lock').checked) $('w').value = Math.max(1, Math.round(value('h') * info.width / info.height));
    };
    return () => ({ tool: 'resize-image', width: value('w'), height: value('h') });
  },
  'edit-image'() {
    row(slider('brightness', 'Brightness', -100, 100, 1, 0) + slider('contrast', 'Contrast', -100, 100, 1, 0));
    row(slider('saturation', 'Saturation', -100, 100, 1, 0) + slider('hue', 'Hue', -180, 180, 1, 0));
    row(slider('sharpen', 'Sharpen', 0, 5, 0.1, 0) + slider('blurAmount', 'Blur', 0, 10, 0.1, 0) + slider('grain', 'Grain', 0, 2, 0.05, 0));
    const preview = () => {
      // An approximation for live feedback; the saved file is made by ImageMagick.
      $('image').style.filter = `brightness(${1 + value('brightness') / 100}) contrast(${1 + value('contrast') / 100})
        saturate(${1 + value('saturation') / 100}) hue-rotate(${value('hue')}deg) blur(${value('blurAmount')}px)`;
    };
    panel.querySelectorAll('input').forEach(input => { input.oninput = preview; });
    return () => ({
      tool: 'adjust-image',
      brightness: value('brightness'),
      contrast: value('contrast'),
      saturation: value('saturation'),
      hue: value('hue'),
      sharpen: value('sharpen'),
      blur: value('blurAmount'),
      grain: value('grain'),
    });
  },
  background() {
    let image = null;
    row(`<label>Colour <input id="color" type="color" value="#ffffff"></label>
         <label>Aspect <select id="ratio"><option value="0">Same as image</option><option value="1">1:1</option>
         <option value="1.7778">16:9</option><option value="0.5625">9:16</option><option value="0.8">4:5</option></select></label>`);
    row(`${numberInput('padding', 'Spacing', Math.round(info.width / 10), 1)}${numberInput('radius', 'Corners', 0, 1)}
         <label><input id="shadow" type="checkbox"> Shadow</label>`);
    row('<button id="pick">Use an image instead</button><span id="picked">Solid colour</span>');
    $('pick').onclick = async () => {
      image = await invoke('editor_pick_image');
      $('picked').textContent = image || 'Solid colour';
    };
    return () => ({
      tool: 'background-image',
      color: $('color').value,
      image,
      ratio: value('ratio'),
      padding: value('padding'),
      radius: value('radius'),
      shadow: $('shadow').checked,
    });
  },
  'redact-image'() {
    const regions = [];
    row('Draw on the image to add an area. <label><input id="blur" type="checkbox" checked> Blur (off for solid black)</label>');
    const render = listRow(regions, area => `${area.width}x${area.height} at ${area.x},${area.y}, ${area.blur ? 'blur' : 'black'}`);
    const add = rectangle => {
      regions.push({ ...rectangle, blur: $('blur').checked });
      render();
    };
    detectRow(add, () => null);
    drawRectangles(add);
    return () => ({ tool: 'redact-image', regions });
  },
  annotate() {
    const shapes = [];
    const size = Math.max(Math.round(info.width / 40), 12);
    row(`Tool: <select id="mark"><option value="arrow">Arrow</option><option value="box">Box</option>
         <option value="ellipse">Ellipse</option><option value="highlight">Highlight</option>
         <option value="text">Text</option><option value="number">Numbered label</option></select>
         <label>Colour <input id="color" type="color" value="#ff0000"></label>
         <input id="words" placeholder="Text to place">
         <button id="undo">Undo</button>`);
    overlay.style.display = 'block';
    const svg = svgElement('svg', { viewBox: `0 0 ${info.width} ${info.height}` });
    overlay.append(svg);
    const point = event => {
      const bounds = overlay.getBoundingClientRect();
      const scale = info.width / bounds.width;
      return {
        x: Math.round((event.clientX - bounds.left) * scale),
        y: Math.round((event.clientY - bounds.top) * scale),
      };
    };
    let draft = null;
    let nextNumber = 1;
    overlay.onmousedown = event => {
      const kind = $('mark').value;
      const color = $('color').value;
      const at = point(event);
      if (kind === 'text') {
        if ($('words').value) shapes.push({ kind, x: at.x, y: at.y, text: $('words').value, color, size });
      } else if (kind === 'number') {
        shapes.push({ kind, x: at.x, y: at.y, value: nextNumber, color, size });
        nextNumber += 1;
      } else {
        draft = { kind, x1: at.x, y1: at.y, x2: at.x, y2: at.y };
        if (kind !== 'highlight') draft.color = color;
        shapes.push(draft);
      }
      renderShapes(svg, shapes);
    };
    overlay.onmousemove = event => {
      if (!draft) return;
      const at = point(event);
      draft.x2 = at.x;
      draft.y2 = at.y;
      renderShapes(svg, shapes);
    };
    window.onmouseup = () => { draft = null; };
    $('undo').onclick = () => {
      const removed = shapes.pop();
      if (removed && removed.kind === 'number') nextNumber -= 1;
      renderShapes(svg, shapes);
    };
    return () => ({ tool: 'annotate-image', shapes });
  },
  organize() {
    const pages = info.pages.map((thumbnail, index) => ({ page: index + 1, rotate: 0, thumbnail }));
    const grid = document.createElement('div');
    grid.id = 'pages';
    panel.append(grid);
    const button = (label, action) => {
      const element = document.createElement('button');
      element.textContent = label;
      element.onclick = () => { action(); render(); };
      return element;
    };
    function render() {
      grid.replaceChildren(...pages.map((entry, index) => {
        const card = document.createElement('div');
        card.className = 'page';
        const image = document.createElement('img');
        image.src = info.urlFor(entry.thumbnail);
        image.style.transform = `rotate(${entry.rotate}deg)`;
        const label = document.createElement('div');
        label.textContent = `Page ${entry.page}`;
        card.append(
          image,
          label,
          button('<', () => { if (index > 0) pages.splice(index - 1, 0, pages.splice(index, 1)[0]); }),
          button('>', () => { if (index < pages.length - 1) pages.splice(index + 1, 0, pages.splice(index, 1)[0]); }),
          button('Rotate', () => { entry.rotate = (entry.rotate + 90) % 360; }),
          button('Duplicate', () => { pages.splice(index + 1, 0, { ...entry }); }),
          button('Remove', () => { pages.splice(index, 1); }),
        );
        return card;
      }));
    }
    render();
    return () => ({ tool: 'organize-pdf', pages: pages.map(({ page, rotate }) => ({ page, rotate })) });
  },
  'compress-to'() {
    row(`Current size: ${(info.bytes / 1e6).toFixed(2)} MB`);
    row(`<label><input type="radio" name="how" value="percent" checked> Percent of current size</label>
         ${numberInput('percent', '', 50, 1)}`);
    row(`<label><input type="radio" name="how" value="size"> Target size in MB</label>
         ${numberInput('target', '', 1, 0.1)}`);
    row(numberInput('side', 'Longest side in pixels, 0 to keep', 0, 1));
    row(`Presets: <button data-preset="80">Light</button><button data-preset="50">Medium</button><button data-preset="25">Strong</button>`);
    panel.querySelectorAll('[data-preset]').forEach(button => {
      button.onclick = () => {
        $('percent').value = button.dataset.preset;
        document.querySelector('[value=percent]').checked = true;
      };
    });
    return () => {
      const bySize = document.querySelector('[value=size]').checked;
      return {
        tool: 'compress-to',
        plan: {
          percent: bySize ? null : value('percent'),
          targetMb: bySize ? value('target') : null,
          maxSide: value('side') > 0 ? value('side') : null,
        },
      };
    };
  },
  normalize() {
    row(`${numberInput('loudness', 'Loudness (LUFS)', -16, 1)}${numberInput('peak', 'True peak (dB)', -1.5, 0.1)}
         ${numberInput('range', 'Loudness range', 11, 1)}`);
    panel.querySelectorAll('input').forEach(input => { input.removeAttribute('min'); });
    row(`Presets: <button data-lufs="-14">Streaming</button><button data-lufs="-16">Podcast</button><button data-lufs="-23">Broadcast</button>`);
    panel.querySelectorAll('[data-lufs]').forEach(button => {
      button.onclick = () => { $('loudness').value = button.dataset.lufs; };
    });
    return () => ({ tool: 'normalize', loudness: value('loudness'), peak: value('peak'), range: value('range') });
  },
  metadata() {
    const table = document.createElement('table');
    info.fields.forEach((field, index) => {
      const line = table.insertRow();
      line.insertCell().textContent = field.name;
      const input = document.createElement('input');
      input.id = `field${index}`;
      input.value = field.value;
      line.insertCell().append(input);
    });
    panel.append(table);
    row('<button id="removeAll">Remove all metadata</button>');
    $('removeAll').onclick = () => { $('removeAll').dataset.clear = '1'; $('apply').click(); };
    return () => ($('removeAll').dataset.clear
      ? { tool: 'remove-metadata' }
      : { tool: 'metadata', fields: info.fields.map((field, index) => ({ name: field.name, value: $(`field${index}`).value })) });
  },
  collage() {
    row(`Layout: <select id="layout"><option value="grid">Grid</option><option value="row">Row</option>
         <option value="column">Column</option><option value="featured">Featured</option></select>
         ${numberInput('spacing', 'Spacing', 16, 1)}
         <label>Background <input id="color" type="color" value="#ffffff"></label>`);
    row(info.names.join(', '));
    return () => ({ layout: $('layout').value, spacing: value('spacing'), background: $('color').value });
  },
  'trim-video'() {
    rangeRow();
    return () => ({ tool: 'trim-video', start: value('start'), end: value('end') });
  },
  'trim-audio'() {
    rangeRow();
    row('<label><input id="silence" type="checkbox"> Only remove silence at the start and end</label>');
    return () => ($('silence').checked
      ? { tool: 'trim-silence' }
      : { tool: 'trim-audio', start: value('start'), end: value('end') });
  },
  crop() {
    return cropPanel('crop-video');
  },
  speed() {
    row(`${numberInput('factor', 'Speed', 2, 0.05)}
         <button data-speed="0.5">0.5x</button><button data-speed="1.5">1.5x</button><button data-speed="2">2x</button>`);
    panel.querySelectorAll('[data-speed]').forEach(button => {
      button.onclick = () => { $('factor').value = button.dataset.speed; };
    });
    return () => ({ tool: 'speed-video', factor: value('factor') });
  },
  'split-video'() {
    const points = [];
    const render = listRow(points, fmt);
    const add = point => { if (!points.includes(point)) points.push(point); points.sort((a, b) => a - b); render(); };
    panel.prepend(row(`<button id="addPoint">Split at playhead</button>
       ${numberInput('parts', 'Equal parts', 2, 1)}<button id="equal">Use equal parts</button>`));
    $('addPoint').onclick = () => add(Number(fmt(player.currentTime)));
    $('equal').onclick = () => {
      points.length = 0;
      for (let part = 1; part < value('parts'); part += 1) add(Number(fmt(info.duration * part / value('parts'))));
    };
    return () => ({ tool: 'split-video', points });
  },
  frames() {
    const times = [];
    const render = listRow(times, fmt);
    panel.prepend(row('<button id="addFrame">Add this frame</button>'));
    $('addFrame').onclick = () => { times.push(Number(fmt(player.currentTime))); render(); };
    return () => ({ tool: 'save-frames', times });
  },
  redact() {
    const areas = [];
    row(`Draw on the video to add an area. ${numberInput('start', 'From', 0)}${numberInput('end', 'To', fmt(info.duration))}
         <label><input id="blur" type="checkbox" checked> Blur (off for solid black)</label>`);
    const render = listRow(areas, area =>
      `${area.width}x${area.height} at ${area.x},${area.y}, ${fmt(area.start)}s to ${fmt(area.end)}s, ${area.blur ? 'blur' : 'black'}`);
    const add = rectangle => {
      areas.push({ ...rectangle, start: value('start'), end: value('end'), blur: $('blur').checked });
      render();
    };
    detectRow(add, () => player.currentTime);
    drawRectangles(add);
    return () => ({ tool: 'redact-video', areas });
  },
  channels() {
    row('<label>Left <input id="left" type="range" min="0" max="2" step="0.05" value="1"></label>');
    row('<label>Right <input id="right" type="range" min="0" max="2" step="0.05" value="1"></label>');
    row('<label><input id="mono" type="checkbox"> Mix down to mono</label>');
    return () => ({ tool: 'channels', left: value('left'), right: value('right'), mono: $('mono').checked });
  },
  bleep() {
    const ranges = [];
    rangeRow();
    panel.append(row('<button id="addBleep">Add bleep</button>'));
    const render = listRow(ranges, ([a, b]) => `${fmt(a)}s to ${fmt(b)}s`);
    $('addBleep').onclick = () => { ranges.push([value('start'), value('end')]); render(); };
    return () => ({ tool: 'bleep', ranges });
  },
  visualize() {
    let image = null;
    row(`Shape: <label><input type="radio" name="shape" value="landscape" checked> Landscape</label>
         <label><input type="radio" name="shape" value="portrait"> Portrait</label>
         <label><input type="radio" name="shape" value="square"> Square</label>`);
    row('<button id="pick">Use a still image</button><span id="picked">Animated waveform</span>');
    $('pick').onclick = async () => {
      image = await invoke('editor_pick_image');
      $('picked').textContent = image || 'Animated waveform';
    };
    return () => ({
      tool: 'visualize',
      image,
      shape: document.querySelector('[name=shape]:checked').value,
    });
  },
};

/// Shows the right kind of preview for what is being edited.
function setupStage() {
  if (info.kind === 'media') {
    $('image').style.display = 'none';
    setupPlayer();
    return;
  }
  $('transport').style.display = 'none';
  $('video').style.display = 'none';
  if (info.kind === 'image') $('image').src = info.previewUrl;
  else $('stage').style.display = 'none';
}

function setupPlayer() {
  const isVideo = info.width > 0;
  player = isVideo ? $('video') : $('audio');
  player.src = info.previewUrl;
  if (!isVideo) $('stage').style.display = 'none';
  if (info.waveform) {
    const wave = $('wave');
    wave.src = info.urlFor(info.waveform);
    wave.style.display = 'block';
    wave.onclick = event => {
      player.currentTime = info.duration * event.offsetX / wave.clientWidth;
    };
  }
  const scrub = $('scrub');
  scrub.max = info.duration;
  player.ontimeupdate = () => {
    scrub.value = player.currentTime;
    $('time').textContent = fmt(player.currentTime);
  };
  scrub.oninput = () => { player.currentTime = Number(scrub.value); };
  $('play').onclick = () => { if (player.paused) player.play(); else player.pause(); };
  player.onplay = () => { $('play').textContent = 'Pause'; };
  player.onpause = () => { $('play').textContent = 'Play'; };
  $('back').onclick = () => { player.pause(); player.currentTime = Math.max(0, player.currentTime - FRAME); };
  $('forward').onclick = () => { player.pause(); player.currentTime += FRAME; };
}

async function start() {
  try {
    info = await invoke('editor_info');
  } catch (error) {
    $('title').textContent = 'Could not open this file';
    $('status').textContent = String(error);
    return;
  }
  info.urlFor = info.urlFor || convertFileSrc;
  info.previewUrl = info.preview ? info.urlFor(info.preview) : '';
  $('title').textContent = info.kind === 'media' ? `${info.name} (${fmt(info.duration)}s)` : info.name;
  setupStage();
  const build = TOOLS[info.tool];
  if (!build) {
    $('status').textContent = `No editor for ${info.tool}`;
    return;
  }
  const collect = build();
  $('apply').onclick = async () => {
    $('status').textContent = '';
    try {
      await invoke('editor_apply', { edit: collect() });
    } catch (error) {
      $('status').textContent = String(error);
    }
  };
}

start();
