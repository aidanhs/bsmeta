# Released by rorekk in Ruby under the MIT license, modified and rewritten by aidanhs

import itertools
import json
import sys

RED_NOTE_TYPE = 0
BLUE_NOTE_TYPE = 1
BOMB_NOTE_TYPE = 3

UP_NOTE_CUT_DIRECTION = 0
DOWN_NOTE_CUT_DIRECTION = 1
LEFT_NOTE_CUT_DIRECTION = 2
RIGHT_NOTE_CUT_DIRECTION = 3
UP_LEFT_NOTE_CUT_DIRECTION = 4
UP_RIGHT_NOTE_CUT_DIRECTION = 5
DOWN_LEFT_NOTE_CUT_DIRECTION = 6
DOWN_RIGHT_NOTE_CUT_DIRECTION = 7
DOT_NOTE_CUT_DIRECTION = 8

TOP_LINE_LAYER = 2
MIDDLE_LINE_LAYER = 1
BOTTOM_LINE_LAYER = 0

def get_info_from_json(json):
  current_time = None
  current_time_blocks = []
  clap_pattern_times = set()
  impossible_pattern_times = set()
  eye_level_note_count = 0

  for note in json.get('_notes', []):
    if current_time != note['_time']:
      current_time_blocks = []
    current_time_blocks.append(note)
    current_time = note['_time']

    note_is_in_middle = note['_lineIndex'] == 1 or note['_lineIndex'] == 2
    if note_is_in_middle and note['_lineLayer'] == MIDDLE_LINE_LAYER:
      eye_level_note_count += 1

    if note['_type'] == BOMB_NOTE_TYPE:
      continue

    if current_time not in clap_pattern_times and len(current_time_blocks) >= 2:
      for block1, block2 in itertools.permutations(current_time_blocks, 2):
        up_clap = block1['_cutDirection'] == UP_RIGHT_NOTE_CUT_DIRECTION and block2['_cutDirection'] == UP_LEFT_NOTE_CUT_DIRECTION
        mid_clap = block1['_cutDirection'] == RIGHT_NOTE_CUT_DIRECTION and block2['_cutDirection'] == LEFT_NOTE_CUT_DIRECTION
        down_clap = block1['_cutDirection'] == DOWN_RIGHT_NOTE_CUT_DIRECTION and block2['_cutDirection'] == DOWN_LEFT_NOTE_CUT_DIRECTION
        is_clap = up_clap or mid_clap or down_clap

        has_clap = (is_clap and
            block1['_lineLayer'] == block2['_lineLayer'] and
            block2['_lineIndex'] == block1['_lineIndex'] + 1)
        if has_clap:
            clap_pattern_times.add(current_time)
            break

    if current_time not in impossible_pattern_times and len(current_time_blocks) >= 2:
      for block1, block2 in itertools.permutations(current_time_blocks, 2):
        is_impossible_pattern = (
          block1['_lineLayer'] == block2['_lineLayer'] and
          block2['_lineIndex'] == block1['_lineIndex'] + 1 and
          block1['_cutDirection'] == LEFT_NOTE_CUT_DIRECTION and
          block2['_cutDirection'] == RIGHT_NOTE_CUT_DIRECTION)
        if is_impossible_pattern:
          impossible_pattern_times.add(current_time)
          break

  return {
    "clap_pattern_count": len(clap_pattern_times),
    "impossible_pattern_count": len(impossible_pattern_times),
    "eye_level_note_count": eye_level_note_count,
  }

infodat = json.load(open('/data/info.dat'))
claps = []
impossible = []
eyelevel = []
for diff_set in infodat['_difficultyBeatmapSets']:
    set_name = diff_set['_beatmapCharacteristicName']
    for diff in diff_set['_difficultyBeatmaps']:
        d = diff['_difficulty']
        diff_json = json.load(open('/data/' + diff['_beatmapFilename']))
        res = get_info_from_json(diff_json)
        print('{}: {}'.format(d, res), file=sys.stderr)
        if res['clap_pattern_count'] > 0:
            claps.append(d)
        if res['impossible_pattern_count'] > 0:
            impossible.append(d)
        if res['eye_level_note_count'] > 0:
            eyelevel.append(d)

print(json.dumps({
    'hasclaps': len(claps) > 0,
    'clapsinfo': ', '.join(claps),
    'hasimpossible': len(impossible) > 0,
    'impossibleinfo': ', '.join(impossible),
    'haseyelevel': len(eyelevel) > 0,
    'eyelevelinfo': ', '.join(eyelevel),
}))
