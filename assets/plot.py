import sys

import subprocess

import matplotlib as mpl
from matplotlib.lines import Line2D
import matplotlib.pyplot as plt
import numpy as np

fig, axs = plt.subplots(1, 2, sharey=True, tight_layout=True)

prev_success = []
prev_failure = []
prev_rsl_success = []
prev_rsl_failure = []
for line in subprocess.run(
    ['cargo', 'run', '--release', '--', '--github-user=fenhl', *sys.argv[1:], 'bench', '--raw-data', '--uncompressed'],
    stdout=subprocess.PIPE, encoding='utf-8', check=True,
).stdout.splitlines():
    kind, rest = line.split(' ', 1)
    match kind:
        case 's':
            prev_success.append(int(rest))
        case 'f':
            prev_failure.append(int(rest))
        case 'S':
            prev_rsl_success.append(int(rest))
        case 'F':
            prev_rsl_failure.append(int(rest))

latest_success = []
latest_failure = []
latest_rsl_success = []
latest_rsl_failure = []
for line in subprocess.run(
    ['cargo', 'run', '--release', '--', '--github-user=fenhl', '--branch=riir', *sys.argv[1:], 'bench', '--raw-data', '--uncompressed'],
    stdout=subprocess.PIPE, encoding='utf-8', check=True,
).stdout.splitlines():
    kind, rest = line.split(' ', 1)
    match kind:
        case 's':
            latest_success.append(int(rest))
        case 'f':
            latest_failure.append(int(rest))
        case 'S':
            latest_rsl_success.append(int(rest))
        case 'F':
            latest_rsl_failure.append(int(rest))

def draw_plot(prev_success, prev_failure, latest_success, latest_failure, path):
    num_bins = int(np.ceil(np.sqrt((len(prev_success) + len(prev_failure) + len(latest_success) + len(latest_failure)) / 2)))
    bins = np.linspace(0, max(max(prev_success), max(prev_failure), max(latest_success), max(latest_failure)), num_bins)

    axs[0].hist([prev_success, latest_success], bins, histtype='step', linewidth=2, alpha=0.7, label=['successes (dev-fenhl)', 'successes (riir)'])

    axs[1].hist([prev_failure, latest_failure], bins, histtype='step', linewidth=2, alpha=0.7, label=['failures (dev-fenhl)', 'failures (riir)'])
    axs[1].legend(loc='upper left')

    for ax in axs:
        # Edit legend to get lines as legend keys instead of the default polygons
        # and sort the legend entries in alphanumeric order
        handles, labels = ax.get_legend_handles_labels()
        leg_entries = {}
        for h, label in zip(handles, labels):
            leg_entries[label] = Line2D([0], [0], color=h.get_facecolor()[:-1],
                                        alpha=h.get_alpha(), lw=h.get_linewidth())
        labels, lines = zip(*reversed(leg_entries.items()))
        ax.legend(lines, labels, loc='upper left')

    fig.savefig(path)

draw_plot(prev_success, prev_failure, latest_success, latest_failure, 'assets/plot.svg')
if prev_rsl_success or prev_rsl_failure or latest_rsl_success or latest_rsl_failure:
    draw_plot(prev_rsl_success, prev_rsl_failure, latest_rsl_success, latest_rsl_failure, 'assets/plot-rsl.svg')
